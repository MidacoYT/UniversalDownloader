//! Vérification et installation des versions publiées sur GitHub.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    process::Command,
    sync::mpsc::{self, Receiver, TryRecvError},
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
    install_rx: Option<Receiver<Result<(), String>>>,
}

impl UpdateCheck {
    pub fn new() -> Self {
        let mut state = Self {
            release: None,
            status: "Checking for app updates…".into(),
            rx: None,
            install_rx: None,
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
        self.status = format!("Downloading version {version} installer…");
        std::thread::spawn(move || {
            let result = download_and_launch_installer(&url, &version, &sha256);
            let _ = tx.send(result);
        });
    }

    pub fn is_loading(&self) -> bool {
        self.rx.is_some() || self.install_rx.is_some()
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = self.install_rx.take() {
            match rx.try_recv() {
                Ok(Ok(())) => {
                    self.status =
                        "The installer is running and will restart the app when ready.".into();
                    changed = true;
                }
                Ok(Err(error)) => {
                    self.status = format!("Could not install the update: {error}");
                    changed = true;
                }
                Err(TryRecvError::Empty) => self.install_rx = Some(rx),
                Err(TryRecvError::Disconnected) => {
                    self.status = "The update installer stopped unexpectedly.".into();
                    changed = true;
                }
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
    }
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

    let mut child = Command::new(&installer_path)
        .args([
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/CLOSEAPPLICATIONS",
            "/RESTARTAPPLICATIONS",
        ])
        .spawn()
        .map_err(|error| {
            let _ = fs::remove_file(&installer_path);
            error.to_string()
        })?;
    std::thread::spawn(move || {
        let _ = child.wait();
        let _ = fs::remove_file(installer_path);
    });
    Ok(())
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
