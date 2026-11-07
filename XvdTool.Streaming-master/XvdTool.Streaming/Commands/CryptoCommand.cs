using System.Diagnostics;
using Spectre.Console;
using System.Net.Http.Headers;
using System.Text.Json;
using Spectre.Console.Cli;

namespace XvdTool.Streaming.Commands;

internal abstract class CryptoCommand<T> : XvdCommand<T> where T : CryptoCommandSettings
{
    protected KeyManager KeyManager = default!;

    protected bool Initialize(CryptoCommandSettings settings, out KeyEntry entry)
    {
        Initialize(settings, requiresWriting: true);

        Debug.Assert(XvdFile != null, "XvdFile != null");

        entry = default;

        KeyManager = new KeyManager();

        if (settings.DeviceKey != null)
        {
            KeyManager.LoadDeviceKey(Convert.FromHexString(settings.DeviceKey));
        }

        if (settings.CikPath != null)
        {
            entry = KeyManager.LoadCik(settings.CikPath);
        }
        else if (settings.CikUrl != null)
        {
            entry = KeyManager.LoadCik(DownloadCik(settings.CikUrl));
        }
        else
        {
            KeyManager.LoadCachedKeys();

            var keyId = XvdFile.GetKeyId();
            if (keyId != Guid.Empty)
            {
                if (!KeyManager.TryGetKey(keyId, out entry))
                {
                    ConsoleLogger.WriteErrLine($"Could not find key [bold]{keyId}[/] loaded in key storage.");

                    return false;
                }
            }
        }

        return true;
    }

    public override ValidationResult Validate(CommandContext context, T settings)
    {
        var result = base.Validate(context, settings);

        if (!result.Successful)
            return result;

        if (settings.CikPath != null && !File.Exists(settings.CikPath))
            return ValidationResult.Error("Provided .cik file does not exist.");

        if (settings.CikPath != null && settings.CikUrl != null)
            return ValidationResult.Error("Use either --cik or --cik-url, not both.");

        if (settings.CikUrl != null &&
            (!Uri.TryCreate(settings.CikUrl, UriKind.Absolute, out var cikUri) ||
             cikUri.Scheme != Uri.UriSchemeHttps ||
             !string.Equals(cikUri.Host, "xboxapi.netlify.app", StringComparison.OrdinalIgnoreCase) ||
             !string.IsNullOrEmpty(cikUri.UserInfo)))
        {
            return ValidationResult.Error("CIK URL must be an HTTPS URL on xboxapi.netlify.app.");
        }

        if (settings.DeviceKey != null && (settings.DeviceKey.Length != 32 ||
                                           settings.DeviceKey.All("0123456789ABCDEFabcdef".Contains)))
            return ValidationResult.Error("Provided device key is invalid. Must be 32 hex characters long.");

        return ValidationResult.Success();
    }

    private static byte[] DownloadCik(string url)
    {
        using var client = new HttpClient { Timeout = TimeSpan.FromSeconds(30) };
        var token = Environment.GetEnvironmentVariable("UD_CIK_AUTH_TOKEN");
        if (!string.IsNullOrWhiteSpace(token))
            client.DefaultRequestHeaders.Authorization = new AuthenticationHeaderValue("Bearer", token);

        using var response = client.GetAsync(url).GetAwaiter().GetResult();
        response.EnsureSuccessStatusCode();
        var body = response.Content.ReadAsByteArrayAsync().GetAwaiter().GetResult();
        if (body.Length == 0)
            throw new InvalidDataException("XboxApi returned an empty CIK response.");

        var offset = 0;
        while (offset < body.Length && body[offset] is (byte)' ' or (byte)'\t' or (byte)'\r' or (byte)'\n')
            offset++;
        if (offset < body.Length && body[offset] == (byte)'{')
        {
            using var document = JsonDocument.Parse(body);
            if (!document.RootElement.TryGetProperty("body", out var data) || data.ValueKind != JsonValueKind.String)
                throw new InvalidDataException("XboxApi returned JSON without a base64 'body' field.");
            return Convert.FromBase64String(data.GetString()!);
        }

        return body;
    }
}
