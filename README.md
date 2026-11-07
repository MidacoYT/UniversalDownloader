# Universal Downloader

A native Windows application written in Rust for browsing Xbox games, downloading their PC packages, and managing a local library.

## Download

- Latest Windows installer: <https://github.com/MidacoYT/UniversalDownloader/releases/latest/download/UniversalDownloader-Setup.exe>
- All releases: <https://github.com/MidacoYT/UniversalDownloader/releases>
- Source code: <https://github.com/MidacoYT/UniversalDownloader>

Every commit pushed to `main` builds and publishes a new release containing the Windows x64 installer. The app's `Updates` page checks these releases, verifies the installer's SHA-256 hash, and can launch it to update the application.

## Run

```powershell
cargo run --release
```

The Xbox catalog authenticates with the catalog service, then loads Microsoft game pages with search and platform filters. Game pages show the available information and link to the official Xbox page.

The library scans a local folder selected with the Windows picker or entered manually. The Downloads page separates active transfers, completed files, and errors, and shows catalog artwork, progress, speed, estimated time remaining, pause, resume, and cancel controls. A game's Download button resolves the MSIXVC package through the Microsoft and XboxDownload APIs, downloads it, then passes the XboxApi CIK key URL directly to the customized XvdTool build. The key is read in memory; Universal Downloader does not create a `.cik` file. The library is rescanned after extraction.

The app looks for `Xvd/` and `OnlineFix/` next to the executable, then in the Rust project. `UD_XVD_TOOL_DIR` and `UD_ONLINEFIX_DIR` can point to other folders. XvdTool is distributed as a self-contained binary; no .NET runtime installation is required. Pause, resume, and cancel are available during extraction and registration. If a manifest is present, `wdapp.exe register` runs under Developer Mode; the previous Developer Mode state is restored afterward. The registration result appears in Downloads, and the library is rescanned after the operation.

Download diagnostics are written to `%APPDATA%\UniversalDownloader\app.log`. Signed URLs are logged without their query strings; HTTP error responses include a body excerpt to help with troubleshooting.

The library matches folder names against titles from the Xbox catalog. Recognized folders show the game and publisher, with a button to open the matching game page. Unmatched folders remain visible under their original names.

## Suggest a game

Submit game contributions through a [GitHub issue](https://github.com/MidacoYT/UniversalDownloader/issues/new). Include the exact game title and store, then attach the data requested below. The team will review the files before updating the catalog.

### Xbox / Microsoft Store

1. Make sure you have access to the game on your Xbox/Microsoft account.
2. Use [CikExtractor](https://github.com/LukeFZ/CikExtractor) to extract the CIK from the package installed by the Xbox app.
3. Create an issue with the game title, its **Microsoft Product ID** (for example, `9NG07QJNK38J`), and attach the extracted `.cik` file.

### Epic Games

The scripts in `tools` retrieve BuildPatch manifests and the data needed to reconstruct chunk download URLs.

1. Install Legendary and sign in to the account that owns the game:

   ```powershell
   python -m pip install legendary-gl
   legendary auth
   ```

2. Export manifests for Epic games available to that account:

   ```powershell
   python tools\export_epic_manifests.py --output "Epic_Manifests"
   ```

   The `_resultats.csv` report is saved after each game and retained if the run is interrupted. If the `.manifest` files are already present and you only need to save their Epic identifiers, use `python tools\export_epic_manifests.py --output "Epic_Manifests" --index-only`; existing manifests will not be downloaded again.

3. Retrieve chunk URLs from the exported manifests:

   ```powershell
   python tools\epic_chunk_urls.py `
     --manifest-dir "Epic_Manifests" `
     --output "epic_chunk_urls"
   ```

   The script reads `.manifest` files directly from the folder. `_resultats.csv` supplies the title, `appName`, `namespace`, and `catalog_item_id` exported by Legendary. These identifiers can resolve games that are missing from title searches and the local cache. The script prints an Epic sign-in link, then securely prompts for the `authorizationCode`. It retrieves the CDN information needed to build the URLs. `summary.json`, each `.txt` file, and each chunk JSON are saved as the games are processed, so an interruption preserves results already resolved.

4. Open an issue with the game title and `appName` (shown in `_resultats.csv` or `chunks\<manifest>.json`), then attach the `.manifest`, its `summary.json` entry, the corresponding chunk JSON, and the `.txt` file containing the URLs.
