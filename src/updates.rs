//! Vérification et installation des versions publiées sur GitHub.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    process::Command,
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    time::Duration,
};

const RELEASES_API: &str =
    "https://api.github.com/repos/MidacoYT/UniversalDownloader/releases/latest";
const INSTALLER_NAME: &str = "UniversalDownloader-Setup.exe";

#[derive(Clone, Default)]
pub struct ReleaseInfo {
    pub version: String,
    pub name: String,
    pub url: String,
    pub notes: String,
    pub installer_url: Option<String>,
    pub installer_sha256: Option<String>,
    pub update_available: bool,
}

pub struct UpdateCheck {
    pub release: Option<ReleaseInfo>,
    pub status: String,
    rx: Option<Receiver<Result<Option<ReleaseInfo>, String>>>,
    install_rx: Option<Receiver<InstallEvent>>,
    pub install_progress: Option<(u64, Option<u64>)>,
    restart_requested: bool,
}

enum InstallEvent {
    Progress(u64, Option<u64>),
    Finished(Result<(), String>),
}

impl UpdateCheck {
    pub fn new() -> Self {
        let mut state = Self {
            release: None,
            status: "Checking for app updates…".into(),
            rx: None,
            install_rx: None,
            install_progress: None,
            restart_requested: false,
        };
        state.refresh();
        state
    }

    pub fn refresh(&mut self) {
        if self.rx.is_some() || self.install_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.status = "Checking for app updates…".into();
        std::thread::spawn(move || {
            let result = (|| {
                let client = reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(30))
                    .user_agent("UniversalDownloader")
                    .build()
                    .map_err(|error| error.to_string())?;
                let value: Value = client
                    .get(RELEASES_API)
                    .send()
                    .and_then(|response| response.error_for_status())
                    .and_then(|response| response.json())
                    .map_err(|error| error.to_string())?;
                let tag = value["tag_name"]
                    .as_str()
                    .unwrap_or_default()
                    .trim_start_matches('v');
                if tag.is_empty() {
                    return Ok(None);
                }
                let installer = value["assets"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|asset| asset["name"].as_str() == Some(INSTALLER_NAME));
                let installer_url = installer
                    .and_then(|asset| asset["browser_download_url"].as_str())
                    .map(str::to_owned);
                let installer_sha256 = installer
                    .and_then(|asset| asset["digest"].as_str())
                    .and_then(|digest| digest.strip_prefix("sha256:"))
                    .filter(|digest| {
                        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
                    .map(str::to_ascii_lowercase);
                Ok(Some(ReleaseInfo {
                    version: tag.to_owned(),
                    name: value["name"].as_str().unwrap_or(tag).to_owned(),
                    url: value["html_url"]
                        .as_str()
                        .unwrap_or("https://github.com/MidacoYT/UniversalDownloader/releases")
                        .to_owned(),
                    notes: value["body"].as_str().unwrap_or_default().to_owned(),
                    installer_url,
                    installer_sha256,
                    update_available: false,
                }))
            })();
            let _ = tx.send(result);
        });
    }

    pub fn install_update(&mut self) {
        if self.install_rx.is_some() {
            return;
        }
        let Some(release) = self
            .release
            .as_ref()
            .filter(|release| release.update_available)
        else {
            return;
        };
        let (Some(url), Some(sha256)) = (
            release.installer_url.clone(),
            release.installer_sha256.clone(),
        ) else {
            self.status =
                "The latest release has no installer with a valid SHA-256 digest yet.".into();
            return;
        };
        let version = release.version.clone();
        let (tx, rx) = mpsc::channel();
        self.install_rx = Some(rx);
        self.install_progress = Some((0, None));
        self.status = format!("Downloading version {version} installer…");
        std::thread::spawn(move || {
            let result = download_and_launch_installer(&url, &version, &sha256, &tx);
            let _ = tx.send(InstallEvent::Finished(result));
        });
    }

    pub fn is_loading(&self) -> bool {
        self.rx.is_some() || self.install_rx.is_some()
    }

    pub fn take_restart_requested(&mut self) -> bool {
        std::mem::take(&mut self.restart_requested)
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = self.install_rx.take() {
            let mut finished = false;
            loop {
                match rx.try_recv() {
                    Ok(InstallEvent::Progress(received, total)) => {
                        self.install_progress = Some((received, total));
                        self.status = match total.filter(|total| *total > 0) {
                            Some(total) => format!(
                                "Downloading update: {}%",
                                (received.saturating_mul(100) / total).min(100)
                            ),
                            None => format!("Downloading update: {} MB", received / (1024 * 1024)),
                        };
                        changed = true;
                    }
                    Ok(InstallEvent::Finished(Ok(()))) => {
                        self.status = "Restarting Universal Downloader to install the update…".into();
                        self.install_progress = None;
                        self.restart_requested = true;
                        finished = true;
                        changed = true;
                        break;
                    }
                    Ok(InstallEvent::Finished(Err(error))) => {
                        self.status = format!("Could not install the update: {error}");
                        self.install_progress = None;
                        finished = true;
                        changed = true;
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.status = "The update installer stopped unexpectedly.".into();
                        self.install_progress = None;
                        finished = true;
                        changed = true;
                        break;
                    }
                }
            }
            if !finished {
                self.install_rx = Some(rx);
            }
        }

        if let Some(rx) = self.rx.take() {
            match rx.try_recv() {
                Ok(Ok(Some(mut release))) => {
                    let current = env!("CARGO_PKG_VERSION").trim_start_matches('v');
                    release.update_available = is_newer(&release.version, current);
                    self.status = if release.update_available {
                        if release.installer_url.is_some() && release.installer_sha256.is_some() {
                            format!(
                                "Version {} is available (current: {}).",
                                release.version, current
                            )
                        } else {
                            format!(
                                "Version {} is available, but a verified installer is missing.",
                                release.version
                            )
                        }
                    } else {
                        format!("You have the latest version ({current}).")
                    };
                    self.release = Some(release);
                    changed = true;
                }
                Ok(Ok(None)) => {
                    self.status = "No published release was found.".into();
                    changed = true;
                }
                Ok(Err(error)) => {
                    self.status = format!("Could not check for app updates: {error}");
                    changed = true;
                }
                Err(TryRecvError::Empty) => self.rx = Some(rx),
                Err(TryRecvError::Disconnected) => {
                    self.status = "Update check stopped.".into();
                    changed = true;
                }
            }
        }
        changed
    }
}

fn download_and_launch_installer(
    url: &str,
    version: &str,
    expected_sha256: &str,
    progress: &Sender<InstallEvent>,
) -> Result<(), String> {
    let parsed_url = reqwest::Url::parse(url).map_err(|error| error.to_string())?;
    if parsed_url.scheme() != "https" || parsed_url.host_str() != Some("github.com") {
        return Err("the installer link is not a trusted GitHub HTTPS URL".into());
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15 * 60))
        .user_agent("UniversalDownloader")
        .build()
        .map_err(|error| error.to_string())?;
    let mut response = client
        .get(url)
        .send()
        .and_then(|response| response.error_for_status())
        .map_err(|error| error.to_string())?;
    let final_host = response.url().host_str().unwrap_or_default();
    if !final_host.ends_with(".githubusercontent.com") && final_host != "github.com" {
        return Err("GitHub redirected the installer to an unexpected host".into());
    }

    let safe_version: String = version
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || *character == '.' || *character == '-'
        })
        .collect();
    let installer_path = std::env::temp_dir().join(format!(
        "UniversalDownloader-Setup-{safe_version}-{}.exe",
        std::process::id()
    ));
    let mut installer = File::create(&installer_path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let total = response.content_length();
    let mut received = 0_u64;
    let mut last_report = std::time::Instant::now();
    loop {
        let count = match response.read(&mut buffer) {
            Ok(count) => count,
            Err(error) => {
                drop(installer);
                let _ = fs::remove_file(&installer_path);
                return Err(error.to_string());
            }
        };
        if count == 0 {
            break;
        }
        if let Err(error) = installer.write_all(&buffer[..count]) {
            drop(installer);
            let _ = fs::remove_file(&installer_path);
            return Err(error.to_string());
        }
        hasher.update(&buffer[..count]);
        received = received.saturating_add(count as u64);
        if last_report.elapsed() >= Duration::from_millis(150) {
            let _ = progress.send(InstallEvent::Progress(received, total));
            last_report = std::time::Instant::now();
        }
    }
    let _ = progress.send(InstallEvent::Progress(received, total));
    drop(installer);
    let actual_sha256 = format!("{:x}", hasher.finalize());
    if actual_sha256 != expected_sha256 {
        let _ = fs::remove_file(&installer_path);
        return Err("the installer SHA-256 digest does not match the GitHub release".into());
    }

    let mut file = File::open(&installer_path).map_err(|error| error.to_string())?;
    let mut signature = [0_u8; 2];
    file.read_exact(&mut signature)
        .map_err(|error| error.to_string())?;
    if signature != *b"MZ" {
        let _ = fs::remove_file(&installer_path);
        return Err("the downloaded file is not a Windows installer".into());
    }
    drop(file);

    // Portable copies (not under Program Files) have no installed app to relaunch:
    // keep the installer around and ask the user to run it manually.
    if !is_installed_app() {
        return Err(format!(
            "Universal Downloader is running as a portable copy: run the installer saved at {} manually.",
            installer_path.display()
        ));
    }
    let app_path = std::env::current_exe()
        .map_err(|error| format!("could not find the installed app to restart: {error}"))?;
    let helper = r#"
$ErrorActionPreference = 'Continue'
$parentId = [int]$env:UD_UPDATE_PARENT_PID
try { Wait-Process -Id $parentId -ErrorAction SilentlyContinue } catch {}
$installer = $env:UD_UPDATE_INSTALLER
$app = $env:UD_UPDATE_APP
try {
    # /NORESTART: this helper performs the restart itself so the app starts exactly once.
    Start-Process -FilePath $installer -ArgumentList '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /CLOSEAPPLICATIONS' -Wait -PassThru | Out-Null
} catch {}
if (Test-Path -LiteralPath $app) {
    try { Start-Process -FilePath $app } catch {}
}
Remove-Item -LiteralPath $installer -Force -ErrorAction SilentlyContinue
"#;
    let mut command = Command::new("powershell.exe");
    command
        .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", helper])
        .env("UD_UPDATE_PARENT_PID", std::process::id().to_string())
        .env("UD_UPDATE_INSTALLER", &installer_path)
        .env("UD_UPDATE_APP", &app_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    if let Err(error) = command.spawn() {
        let _ = fs::remove_file(&installer_path);
        return Err(format!("could not start the hidden update/restart helper: {error}"));
    }
    Ok(())
}


/// True when the current executable lives under Program Files (Inno Setup install).
/// A debug build or a manually moved copy is treated as portable.
fn is_installed_app() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let exe = exe.canonicalize().unwrap_or(exe);
    ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"]
        .iter()
        .filter_map(|variable| std::env::var(variable).ok())
        .any(|dir| exe.starts_with(std::path::PathBuf::from(dir)))
}
fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |version: &str| {
        version
            .split('.')
            .map(|part| {
                part.split('-')
                    .next()
                    .unwrap_or("0")
                    .parse::<u64>()
                    .unwrap_or(0)
            })
            .collect::<Vec<_>>()
    };
    let (latest, current) = (parse(latest), parse(current));
    for index in 0..latest.len().max(current.len()) {
        match latest
            .get(index)
            .copied()
            .unwrap_or(0)
            .cmp(&current.get(index).copied().unwrap_or(0))
        {
            std::cmp::Ordering::Greater => return true,
            std::cmp::Ordering::Less => return false,
            std::cmp::Ordering::Equal => {}
        }
    }
    false
}
