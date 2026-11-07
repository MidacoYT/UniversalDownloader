//! Bibliothèque locale : scan des dossiers, association au catalogue, lancement, désinstallation.
use crate::model::Game;
use crate::util::{GIB, normalize_title};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};

#[derive(Clone)]
pub struct LibraryEntry {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub game_id: Option<String>,
}

#[derive(PartialEq, Clone, Copy)]
pub enum LibrarySort {
    Name,
    Size,
}

impl LibrarySort {
    pub const ALL: [Self; 2] = [Self::Name, Self::Size];
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Sort by name",
            Self::Size => "Sort by size",
        }
    }
}

#[derive(PartialEq, Clone, Copy)]
pub enum LibraryView {
    Grid,
    List,
}

/// Une ligne affichable : dossier local + jeu du catalogue éventuel.
pub struct LibraryRow<'a> {
    pub entry: &'a LibraryEntry,
    pub game: Option<&'a Game>,
}

impl LibraryRow<'_> {
    pub fn title(&self) -> &str {
        self.game
            .map(|g| g.name.as_str())
            .unwrap_or(&self.entry.name)
    }
}

type ScanResult = Result<Vec<LibraryEntry>, String>;

pub struct Library {
    pub entries: Vec<LibraryEntry>,
    pub search: String,
    pub sort: LibrarySort,
    pub view: LibraryView,
    pub status: String,
    rx: Option<Receiver<ScanResult>>,
}

impl Library {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            search: String::new(),
            sort: LibrarySort::Name,
            view: LibraryView::Grid,
            status: "Choose the folder where your games are installed.".into(),
            rx: None,
        }
    }

    pub fn is_scanning(&self) -> bool {
        self.rx.is_some()
    }

    pub fn total_bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.bytes).sum()
    }

    pub fn installed_ids(&self) -> HashSet<&str> {
        self.entries
            .iter()
            .filter_map(|e| e.game_id.as_deref())
            .collect()
    }

    pub fn scan(&mut self, root: &str) {
        if self.is_scanning() {
            return;
        }
        let root = PathBuf::from(root.trim());
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.status = "Scanning folder…".into();
        std::thread::spawn(move || {
            let _ = tx.send(scan_folder(&root));
        });
    }

    /// Retourne `true` quand un scan réussi vient de se terminer (il faut ré-associer les jeux).
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        match rx.try_recv() {
            Ok(Ok(entries)) => {
                self.status = format!(
                    "{} folders · {:.1} GB total",
                    entries.len(),
                    entries.iter().map(|e| e.bytes).sum::<u64>() as f64 / GIB
                );
                self.entries = entries;
                self.rx = None;
                true
            }
            Ok(Err(error)) => {
                self.status = error;
                self.rx = None;
                false
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.rx = None;
                false
            }
        }
    }

    /// Associe chaque dossier à un jeu du catalogue.
    pub fn rematch<'a>(&mut self, games: impl IntoIterator<Item = &'a Game>) {
        // Titres normalisés une seule fois (et non une fois par dossier et par jeu).
        let titles: Vec<(String, &Game)> = games
            .into_iter()
            .map(|game| (normalize_title(&game.name), game))
            .filter(|(title, _)| title.len() >= MIN_TITLE_LEN)
            .collect();
        for entry in &mut self.entries {
            entry.game_id = match_installed_game(&entry.name, &titles).map(|g| g.id.clone());
        }
        let supported = self.entries.iter().filter(|entry| entry.game_id.is_some()).count();
        self.status = format!(
            "{supported} supported game{} found in the selected folder.",
            if supported == 1 { "" } else { "s" }
        );
    }

    /// Lignes filtrées et triées pour l'affichage. `catalogs` : tous les catalogues où chercher
    /// le jeu associé (Xbox ET Epic).
    pub fn rows<'a>(&'a self, catalogs: &[&'a [Game]]) -> Vec<LibraryRow<'a>> {
        let filter = self.search.trim().to_lowercase();
        let mut rows: Vec<LibraryRow<'a>> = self
            .entries
            .iter()
            .filter_map(|entry| {
                let game = entry.game_id.as_deref().and_then(|id| {
                    catalogs
                        .iter()
                        .find_map(|games| games.iter().find(|g| g.id == id))
                })?;
                let row = LibraryRow {
                    entry,
                    game: Some(game),
                };
                (entry.name.to_lowercase().contains(&filter)
                    || row.title().to_lowercase().contains(&filter))
                .then_some(row)
            })
            .collect();
        match self.sort {
            LibrarySort::Name => rows.sort_by_cached_key(|r| r.title().to_lowercase()),
            LibrarySort::Size => rows.sort_by(|a, b| b.entry.bytes.cmp(&a.entry.bytes)),
        }
        rows
    }

    /// Supprime le dossier du jeu (uniquement un enfant direct du dossier de bibliothèque).
    pub fn uninstall(&mut self, entry: &LibraryEntry, library_root: &str) {
        match remove_game_folder(entry, library_root) {
            Ok(()) => {
                self.entries.retain(|item| item.path != entry.path);
                self.status = format!("{} game folders found.", self.entries.len());
            }
            Err(error) => self.status = format!("Uninstall failed: {error}"),
        }
    }
}

fn scan_folder(root: &Path) -> ScanResult {
    if !root.is_dir() {
        return Err(format!("Folder not found: {}", root.display()));
    }
    let mut entries = Vec::new();
    for item in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let item = item.map_err(|e| e.to_string())?;
        if item.file_type().map_err(|e| e.to_string())?.is_dir() {
            let path = item.path();
            entries.push(LibraryEntry {
                name: item.file_name().to_string_lossy().into_owned(),
                bytes: folder_size(&path),
                path,
                game_id: None,
            });
        }
    }
    entries.sort_by_key(|e| e.name.to_lowercase());
    Ok(entries)
}

fn folder_size(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let Ok(kind) = entry.file_type() else {
                return 0;
            };
            if kind.is_file() {
                entry.metadata().map(|m| m.len()).unwrap_or(0)
            } else if kind.is_dir() {
                folder_size(&entry.path())
            } else {
                0
            }
        })
        .sum()
}

fn remove_game_folder(entry: &LibraryEntry, library_root: &str) -> Result<(), String> {
    let root = std::fs::canonicalize(library_root.trim())
        .map_err(|e| format!("Could not access the library folder: {e}"))?;
    let target = std::fs::canonicalize(&entry.path)
        .map_err(|e| format!("Could not access the game folder: {e}"))?;
    if target == root || target.parent() != Some(root.as_path()) {
        return Err("The selected folder is outside the configured library.".into());
    }
    if !target.is_dir() {
        return Err("The selected game folder no longer exists.".into());
    }
    unregister_packages_in_folder(&target)?;
    std::fs::remove_dir_all(&target).map_err(|e| format!("Could not remove the game folder: {e}"))
}

/// Retire de Windows les paquets Appx enregistrés depuis ce dossier avant d'en supprimer les
/// fichiers. Les jeux Epic et les dossiers sans paquet enregistré ne sont pas affectés.
#[cfg(windows)]
fn unregister_packages_in_folder(target: &Path) -> Result<(), String> {
    use std::process::{Command, Stdio};

    let script = r#"
$ErrorActionPreference = 'Stop'
$target = [IO.Path]::GetFullPath($env:UD_UNINSTALL_TARGET).TrimEnd('\')
$packages = Get-AppxPackage | Where-Object {
    if (-not $_.InstallLocation) { return $false }
    $location = [IO.Path]::GetFullPath($_.InstallLocation).TrimEnd('\')
    $location.Equals($target, [StringComparison]::OrdinalIgnoreCase) -or
        $location.StartsWith($target + '\', [StringComparison]::OrdinalIgnoreCase)
}
foreach ($package in $packages) {
    Remove-AppxPackage -Package $package.PackageFullName -ErrorAction Stop
}
"#;
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .env("UD_UNINSTALL_TARGET", target)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Could not start Windows package removal: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let details = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(if details.is_empty() {
            format!("Windows package removal exited with {}.", output.status)
        } else {
            format!("Could not unregister the Xbox game from Windows: {details}")
        })
    }
}

#[cfg(not(windows))]
fn unregister_packages_in_folder(_target: &Path) -> Result<(), String> {
    Ok(())
}

/// Longueur minimale d'un titre normalisé pour être associé à un dossier.
const MIN_TITLE_LEN: usize = 5;

/// `titles` : couples (titre normalisé, jeu) déjà filtrés sur `MIN_TITLE_LEN`.
fn match_installed_game<'a>(folder: &str, titles: &[(String, &'a Game)]) -> Option<&'a Game> {
    let folder = normalize_title(folder);
    if folder.is_empty() {
        return None;
    }
    titles
        .iter()
        .filter(|(title, _)| {
            folder == *title || folder.starts_with(title.as_str()) || title.starts_with(&folder)
        })
        .max_by_key(|(title, _)| title.len())
        .map(|(_, game)| *game)
}

/// Lance le premier .exe du dossier (hors utilitaires `wdapp`).
pub fn launch_game(folder: &Path, preferred_exe: Option<&str>) -> Result<(), String> {
    let executable = find_game_executable(folder, preferred_exe)?;

    std::process::Command::new(&executable)
        .current_dir(folder)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Could not launch game: {e}"))
}

fn find_game_executable(folder: &Path, preferred_exe: Option<&str>) -> Result<PathBuf, String> {
    let preferred = preferred_exe.map(|exe| folder.join(exe.replace('/', "\\")));
    if let Some(path) = preferred.filter(|path| path.is_file()) {
        return Ok(path);
    }
    std::fs::read_dir(folder)
        .map_err(|e| format!("Could not read game folder: {e}"))?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
                && !path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().contains("wdapp"))
        })
        .ok_or_else(|| "No game executable was found in this folder.".to_owned())
}

#[cfg(windows)]
pub fn create_desktop_shortcut(
    name: &str,
    folder: &Path,
    store: crate::model::GameStore,
    preferred_exe: Option<&str>,
) -> Result<(), String> {
    use std::process::{Command, Stdio};

    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
        ])
        .env("UD_SHORTCUT_NAME", name)
        .env("UD_SHORTCUT_FOLDER", folder)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let script = match store {
        crate::model::GameStore::Epic => {
            let executable = find_game_executable(folder, preferred_exe)?;
            command
                .env("UD_SHORTCUT_TARGET", &executable)
                .env("UD_SHORTCUT_WORKDIR", folder);
            r#"
$ErrorActionPreference = 'Stop'
$target = $env:UD_SHORTCUT_TARGET
$workdir = $env:UD_SHORTCUT_WORKDIR
$arguments = ''
$icon = $target
"#
        }
        crate::model::GameStore::Xbox => {
            command
                .env("UD_SHORTCUT_FOLDER", folder)
                .env("UD_SHORTCUT_EXECUTABLE", preferred_exe.unwrap_or_default());
            r#"
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath($env:UD_SHORTCUT_FOLDER).TrimEnd('\')
$manifestPath = Join-Path $root 'appxmanifest.xml'
if (-not (Test-Path -LiteralPath $manifestPath)) { throw 'appxmanifest.xml was not found in the Xbox game folder.' }
[xml]$manifest = Get-Content -LiteralPath $manifestPath -Raw
$identity = $manifest.SelectSingleNode("//*[local-name()='Identity']")
if (-not $identity) { throw 'The Xbox app manifest has no package identity.' }
$package = Get-AppxPackage -Name $identity.GetAttribute('Name') | Select-Object -First 1
if (-not $package) { throw 'No registered Xbox package matches this game manifest.' }
$applications = @($manifest.SelectNodes("//*[local-name()='Application']"))
if ($env:UD_SHORTCUT_EXECUTABLE) {
    $preferred = [IO.Path]::GetFileName($env:UD_SHORTCUT_EXECUTABLE)
    $application = $applications | Where-Object { [IO.Path]::GetFileName($_.GetAttribute('Executable')) -ieq $preferred } | Select-Object -First 1
}
if (-not $application) { $application = $applications | Select-Object -First 1 }
if (-not $application -or -not $application.GetAttribute('Id')) { throw 'The Xbox package has no launchable application.' }
$target = Join-Path $env:WINDIR 'explorer.exe'
$arguments = 'shell:AppsFolder\' + $package.PackageFamilyName + '!' + $application.GetAttribute('Id')
$workdir = $root
$icon = $target
"#
        }
    };
    let common = r#"
$desktop = [Environment]::GetFolderPath('DesktopDirectory')
$safeName = [regex]::Replace($env:UD_SHORTCUT_NAME, '[\x00-\x1f<>:"/\\|?*]', '_').Trim().TrimEnd('.')
if ([string]::IsNullOrWhiteSpace($safeName)) { throw 'The game name is not valid for a shortcut.' }
$shortcutPath = Join-Path $desktop ($safeName + '.lnk')
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($shortcutPath)
$shortcut.TargetPath = $target
$shortcut.Arguments = $arguments
$shortcut.WorkingDirectory = $workdir
$shortcut.IconLocation = $icon
$shortcut.Save()
"#;
    let output = command
        .arg(format!("{script}\n{common}"))
        .output()
        .map_err(|error| format!("Could not start shortcut creation: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let details = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(if details.is_empty() {
            format!("PowerShell exited with {}.", output.status)
        } else {
            details
        })
    }
}

#[cfg(not(windows))]
pub fn create_desktop_shortcut(
    _name: &str,
    _folder: &Path,
    _store: crate::model::GameStore,
    _preferred_exe: Option<&str>,
) -> Result<(), String> {
    Err("Desktop shortcuts are supported only on Windows.".into())
}
