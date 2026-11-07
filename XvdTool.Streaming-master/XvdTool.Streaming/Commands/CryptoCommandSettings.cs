using System.ComponentModel;
using Spectre.Console.Cli;

namespace XvdTool.Streaming.Commands;

internal abstract class CryptoCommandSettings : XvdCommandSettings
{
    [Description("Path to the .cik file to be used regardless of the header key ID.")]
    [CommandOption("-c|--cik")]
    public string? CikPath { get; init; }

    [Description("HTTPS URL that returns raw .cik bytes or a JSON object with a base64 'body' field. XboxApi bearer token is read from UD_CIK_AUTH_TOKEN.")]
    [CommandOption("--cik-url")]
    public string? CikUrl { get; init; }

    [Description("Device key used to decrypt UWP licenses.")]
    [CommandOption("-d|--device-key")]
    public string? DeviceKey { get; init; }
}
