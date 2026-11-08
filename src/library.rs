//! Local library: folder scan, catalog matching, launch, uninstall.
use crate::model::Game;
use crate::util::{GIB, normalize_title};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

fn hide_child_console(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
}

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
    uninstall_rx: Option<Receiver<UninstallEvent>>,
    uninstall: Option<UninstallProgress>,
    uninstall_path: Option<PathBuf>,
}

/// Avancement d'une désinstallation en cours (affiché dans la fenêtre de progression).
#[derive(Clone)]
pub struct UninstallProgress {
    pub name: String,
    pub stage: String,
    /// `None` tant que l'étape n'a pas de pourcentage (désenregistrement Windows).
    pub fraction: Option<f32>,
}

enum UninstallEvent {
    Stage(String, Option<f32>),
    Progress(f32),
    Done(Result<(), String>),
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
            uninstall_rx: None,
            uninstall: None,
            uninstall_path: None,
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

    pub fn scan(&mut self, root: &str, excluded_paths: Vec<PathBuf>) {
        let root = PathBuf::from(root.trim());
        let excluded_paths: HashSet<String> =
            excluded_paths.iter().map(|path| path_key(path)).collect();
        self.entries
            .retain(|entry| !excluded_paths.contains(&path_key(&entry.path)));
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.status = "Scanning folder…".into();
        std::thread::spawn(move || {
            let _ = tx.send(scan_folder(&root, &excluded_paths));
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
        let supported = self
            .entries
            .iter()
            .filter(|entry| entry.game_id.is_some())
            .count();
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
    pub fn is_uninstalling(&self) -> bool {
        self.uninstall_rx.is_some()
    }

    pub fn uninstall_progress(&self) -> Option<&UninstallProgress> {
        self.uninstall.as_ref()
    }

    /// Lance la désinstallation dans un thread : l'interface reste fluide et affiche l'avancement.
    pub fn start_uninstall(&mut self, entry: LibraryEntry, library_root: &str) {
        if self.is_uninstalling() {
            self.status = "Another uninstall is already in progress.".into();
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.uninstall_rx = Some(rx);
        self.uninstall_path = Some(entry.path.clone());
        self.uninstall = Some(UninstallProgress {
            name: entry.name.clone(),
            stage: "Preparing…".into(),
            fraction: None,
        });
        let library_root = library_root.to_owned();
        std::thread::spawn(move || {
            let result = run_uninstall(&entry, &library_root, &tx);
            let _ = tx.send(UninstallEvent::Done(result));
        });
    }

    /// Retourne `true` quand une désinstallation vient de réussir (il faut ré-associer les jeux).
    pub fn poll_uninstall(&mut self) -> bool {
        let Some(rx) = &self.uninstall_rx else {
            return false;
        };
        let mut finished = None;
        loop {
            match rx.try_recv() {
                Ok(UninstallEvent::Stage(stage, fraction)) => {
                    if let Some(progress) = &mut self.uninstall {
                        progress.stage = stage;
                        progress.fraction = fraction;
                    }
                }
                Ok(UninstallEvent::Progress(fraction)) => {
                    if let Some(progress) = &mut self.uninstall {
                        progress.fraction = Some(fraction);
                    }
                }
                Ok(UninstallEvent::Done(result)) => {
                    finished = Some(result);
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    finished = Some(Err("The uninstall stopped unexpectedly.".into()));
                    break;
                }
            }
        }
        let Some(result) = finished else {
            return false;
        };
        let path = self.uninstall_path.take();
        self.uninstall_rx = None;
        self.uninstall = None;
        match result {
            Ok(()) => {
                if let Some(path) = path {
                    remove_desktop_shortcuts(&path);
                    self.entries.retain(|item| item.path != path);
                }
                self.status = format!("{} game folders found.", self.entries.len());
                true
            }
            Err(error) => {
                self.status = format!("Uninstall failed: {error}");
                false
            }
        }
    }
}

fn scan_folder(root: &Path, excluded_paths: &HashSet<String>) -> ScanResult {
    if !root.is_dir() {
        return Err(format!("Folder not found: {}", root.display()));
    }
    let mut entries = Vec::new();
    for item in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let item = item.map_err(|e| e.to_string())?;
        if item.file_type().map_err(|e| e.to_string())?.is_dir() {
            let path = item.path();
            if excluded_paths.contains(&path_key(&path)) {
                continue;
            }
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

fn path_key(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_lowercase()
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

fn run_uninstall(
    entry: &LibraryEntry,
    library_root: &str,
    tx: &Sender<UninstallEvent>,
) -> Result<(), String> {
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
    let _ = tx.send(UninstallEvent::Stage(
        "Unregistering from Windows…".into(),
        None,
    ));
    unregister_packages_in_folder(&target)?;
    let _ = tx.send(UninstallEvent::Stage("Deleting game files…".into(), Some(0.0)));
    remove_dir_with_progress(&target, |fraction| {
        let _ = tx.send(UninstallEvent::Progress(fraction));
    })
}

fn collect_tree(dir: &Path, files: &mut Vec<PathBuf>, dirs: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for item in std::fs::read_dir(dir)? {
        let item = item?;
        let path = item.path();
        // `file_type` ne suit pas les liens : un lien/jonction est supprimé tel quel.
        if item.file_type()?.is_dir() {
            collect_tree(&path, files, dirs)?;
            dirs.push(path);
        } else {
            files.push(path);
        }
    }
    Ok(())
}

fn remove_file_force(path: &Path) -> std::io::Result<()> {
    std::fs::remove_file(path).or_else(|error| {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            // Fichier en lecture seule : on retire l'attribut puis on réessaie.
            let mut permissions = std::fs::metadata(path)?.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            std::fs::set_permissions(path, permissions)?;
            std::fs::remove_file(path)
        } else if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            // Lien de dossier (jonction) : se supprime comme un dossier vide.
            std::fs::remove_dir(path)
        } else {
            Err(error)
        }
    })
}

/// Supprime le dossier fichier par fichier en signalant l'avancement (0.0 à 1.0). Si une
/// suppression échoue de façon inattendue, on retombe sur `remove_dir_all` comme avant.
fn remove_dir_with_progress(target: &Path, mut report: impl FnMut(f32)) -> Result<(), String> {
    let fallback = |report: &mut dyn FnMut(f32)| {
        std::fs::remove_dir_all(target)
            .map(|()| report(1.0))
            .map_err(|e| format!("Could not remove the game folder: {e}"))
    };
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    if collect_tree(target, &mut files, &mut dirs).is_err() {
        return fallback(&mut report);
    }
    let total = files.len().max(1) as f32;
    for (index, file) in files.iter().enumerate() {
        if remove_file_force(file).is_err() {
            return fallback(&mut report);
        }
        if index % 25 == 0 || index + 1 == files.len() {
            report((index + 1) as f32 / total);
        }
    }
    // Dossiers les plus profonds d'abord.
    dirs.sort_by_key(|dir| std::cmp::Reverse(dir.components().count()));
    for dir in &dirs {
        if std::fs::remove_dir(dir).is_err() {
            return fallback(&mut report);
        }
    }
    std::fs::remove_dir(target).map_err(|e| format!("Could not remove the game folder: {e}"))?;
    report(1.0);
    Ok(())
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
    let mut command = Command::new("powershell.exe");
    command.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        script,
    ]);
    hide_child_console(&mut command);
    let output = command
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

/// Crée le raccourci bureau d'un jeu.
///
/// - Epic : raccourci classique vers l'exécutable du jeu.
/// - Xbox : activation AppX via AppsFolder, avec repli sur l'exécutable du jeu si le package
///   n'est pas enregistré pour l'utilisateur courant.
pub fn create_shortcut(
    name: &str,
    folder: &Path,
    store: crate::model::GameStore,
    preferred_exe: Option<&str>,
    _product_id: Option<&str>,
) -> Result<PathBuf, String> {
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
    hide_child_console(&mut command);

    // Dossier du bureau + nom de fichier du raccourci (commun aux deux boutiques).
    let desktop_prelude = r#"
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
$desktop = [Environment]::GetFolderPath('DesktopDirectory')
if ([string]::IsNullOrWhiteSpace($desktop)) { throw 'Windows did not provide the current user desktop path.' }
New-Item -ItemType Directory -Path $desktop -Force | Out-Null
$safeName = [regex]::Replace($env:UD_SHORTCUT_NAME, '[\x00-\x1f<>:"/\\|?*]', '_').Trim().TrimEnd('.')
if ([string]::IsNullOrWhiteSpace($safeName)) { throw 'The game name is not valid for a shortcut.' }
$shortcutPath = Join-Path $desktop ($safeName + '.lnk')
"#;

    let script = match store {
        crate::model::GameStore::Epic => {
            let executable = find_game_executable(folder, preferred_exe)?;
            command
                .env("UD_SHORTCUT_TARGET", &executable)
                .env("UD_SHORTCUT_WORKDIR", folder);
            format!(
                r#"
$ErrorActionPreference = 'Stop'
$target = $env:UD_SHORTCUT_TARGET
$workdir = $env:UD_SHORTCUT_WORKDIR
$arguments = ''
$icon = $target
{desktop_prelude}
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($shortcutPath)
$shortcut.TargetPath = $target
$shortcut.Arguments = $arguments
$shortcut.WorkingDirectory = $workdir
$shortcut.IconLocation = $icon
$shortcut.Save()
if (-not (Test-Path -LiteralPath $shortcutPath -PathType Leaf)) {{ throw 'Windows did not create the desktop shortcut file.' }}
$shortcutPath
"#
            )
        }
        crate::model::GameStore::Xbox => {
            let manifest_path = folder.join("appxmanifest.xml");
            if !manifest_path.is_file() {
                return Err(format!(
                    "The Xbox package manifest was not found in {}.",
                    folder.display()
                ));
            }
            command
                .env("UD_SHORTCUT_EXECUTABLE", preferred_exe.unwrap_or_default());
            format!(
                r#"
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath($env:UD_SHORTCUT_FOLDER).TrimEnd('\')
$manifestPath = Join-Path $root 'appxmanifest.xml'
[xml]$manifest = Get-Content -LiteralPath $manifestPath -Raw
$identity = $manifest.SelectSingleNode("//*[local-name()='Identity']")
if (-not $identity) {{ throw 'The Xbox app manifest has no package identity.' }}
$applications = @($manifest.SelectNodes("//*[local-name()='Application']"))
if ($env:UD_SHORTCUT_EXECUTABLE) {{
    $preferred = [IO.Path]::GetFileName($env:UD_SHORTCUT_EXECUTABLE)
    $application = $applications | Where-Object {{ [IO.Path]::GetFileName($_.GetAttribute('Executable')) -ieq $preferred }} | Select-Object -First 1
}}
if (-not $application) {{ $application = $applications | Select-Object -First 1 }}
if (-not $application -or -not $application.GetAttribute('Id')) {{ throw 'The Windows package has no launchable application.' }}
$appId = $application.GetAttribute('Id')

# Icône : le logo du jeu s'il existe, sinon l'exécutable du jeu.
$icon = ''
$logo = Join-Path $root 'Content\SmallLogo.scale-200.ico'
if (Test-Path -LiteralPath $logo -PathType Leaf) {{
    $icon = $logo
}} else {{
    $exeName = $application.GetAttribute('Executable')
    if ($exeName) {{
        $exePath = Join-Path $root $exeName
        if (Test-Path -LiteralPath $exePath -PathType Leaf) {{ $icon = $exePath }}
    }}
}}
{desktop_prelude}
$shortcutPath = Join-Path $desktop ($safeName + '.lnk')
$packageName = $identity.GetAttribute('Name')
$package = Get-AppxPackage -Name $packageName | Where-Object {{
    if (-not $_.InstallLocation) {{ return $false }}
    $location = [IO.Path]::GetFullPath($_.InstallLocation).TrimEnd('\')
    $location.Equals($root, [StringComparison]::OrdinalIgnoreCase) -or
        $location.StartsWith($root + '\', [StringComparison]::OrdinalIgnoreCase)
}} | Select-Object -First 1
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($shortcutPath)
$explorerPath = Join-Path $env:WINDIR 'explorer.exe'
$shortcut.TargetPath = $explorerPath
if ($package) {{
    $shortcut.Arguments = 'shell:AppsFolder\' + $package.PackageFamilyName + '!' + $appId
    $shortcut.WorkingDirectory = ''
}} else {{
    $exeName = $application.GetAttribute('Executable')
    $exePath = if ($exeName) {{ Join-Path $root $exeName }} else {{ '' }}
    if ([string]::IsNullOrWhiteSpace($exePath) -or -not (Test-Path -LiteralPath $exePath -PathType Leaf)) {{
        throw "The Xbox package is not registered and its executable was not found: $exePath"
    }}
    $shortcut.TargetPath = $exePath
    $shortcut.Arguments = ''
    $shortcut.WorkingDirectory = $root
}}
if (-not [string]::IsNullOrWhiteSpace($icon)) {{ $shortcut.IconLocation = $icon }}
$shortcut.WindowStyle = 1
$shortcut.Save()
if (-not (Test-Path -LiteralPath $shortcutPath -PathType Leaf)) {{ throw 'Windows did not create the desktop shortcut file.' }}
Remove-Item -LiteralPath (Join-Path $desktop ($safeName + '.url')) -Force -ErrorAction SilentlyContinue
$shortcutPath
"#
            )
        }
    };

    let output = command
        .arg(script)
        .output()
        .map_err(|error| format!("Could not start shortcut creation: {error}"))?;
    if output.status.success() {
        let shortcut_path = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(PathBuf::from)
            .ok_or("Windows did not report where it created the shortcut.")?;
        if shortcut_path.is_file() {
            Ok(shortcut_path)
        } else {
            Err(format!(
                "The desktop shortcut was not found at {}.",
                shortcut_path.display()
            ))
        }
    } else {
        let details = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(if details.is_empty() {
            format!("PowerShell exited with {}.", output.status)
        } else {
            details
        })
    }
}

/// Supprime, en arrière-plan, les raccourcis du bureau qui pointent vers le dossier d'un jeu
/// (cible, dossier de travail ou icône dans ce dossier) : ceux créés à l'installation.
#[cfg(windows)]
pub fn remove_desktop_shortcuts(folder: &Path) {
    let folder = folder.to_path_buf();
    std::thread::spawn(move || {
        let mut command = std::process::Command::new("powershell.exe");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                r#"
$ErrorActionPreference = 'SilentlyContinue'
$root = [IO.Path]::GetFullPath($env:UD_SHORTCUT_FOLDER).TrimEnd('\')
$desktop = [Environment]::GetFolderPath('DesktopDirectory')
if ([string]::IsNullOrWhiteSpace($desktop)) { exit 0 }
$shell = New-Object -ComObject WScript.Shell
function Test-Inside($path) {
    if ([string]::IsNullOrWhiteSpace($path)) { return $false }
    $full = [IO.Path]::GetFullPath($path).TrimEnd('\')
    return ($full -ieq $root) -or $full.StartsWith($root + '\', [StringComparison]::OrdinalIgnoreCase)
}
Get-ChildItem -LiteralPath $desktop -File | Where-Object { $_.Extension -in '.lnk', '.url' } | ForEach-Object {
    if ($_.Extension -ieq '.lnk') {
        $link = $shell.CreateShortcut($_.FullName)
        $iconPath = [regex]::Replace([string]$link.IconLocation, ',\s*-?\d+$', '')
        if ((Test-Inside $link.WorkingDirectory) -or (Test-Inside $link.TargetPath) -or (Test-Inside $iconPath)) {
            Remove-Item -LiteralPath $_.FullName -Force
        }
    } else {
        $urlFile = Get-Content -LiteralPath $_.FullName -Raw
        $iconMatch = [regex]::Match($urlFile, '(?im)^IconFile=(.+)$')
        if ($iconMatch.Success -and (Test-Inside $iconMatch.Groups[1].Value.Trim())) {
            Remove-Item -LiteralPath $_.FullName -Force
        }
    }
}
"#,
            ])
            .env("UD_SHORTCUT_FOLDER", &folder)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        hide_child_console(&mut command);
        let _ = command.status();
    });
}

#[cfg(not(windows))]
pub fn remove_desktop_shortcuts(_folder: &Path) {}

#[cfg(not(windows))]
pub fn create_shortcut(
    _name: &str,
    _folder: &Path,
    _store: crate::model::GameStore,
    _preferred_exe: Option<&str>,
    _product_id: Option<&str>,
) -> Result<PathBuf, String> {
    Err("Desktop shortcuts are supported only on Windows.".into())
}
