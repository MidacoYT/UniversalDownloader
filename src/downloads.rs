//! Téléchargements HTTP(S) directs, indépendants du catalogue Xbox.
use crate::model::Game;
use reqwest::Url;
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, RANGE, USER_AGENT};
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const XBOX_RESOLVER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36";
const XBOX_ASSET_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";
const MAX_DOWNLOAD_READ_RETRIES: u8 = 8;

fn download_log(message: impl AsRef<str>) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs())
        .unwrap_or_default();
    let line = format!("[{timestamp}][downloads] {}\n", message.as_ref());
    let _ = std::io::stderr().write_all(line.as_bytes());
    let root = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let directory = root.join("UniversalDownloader");
    if fs::create_dir_all(&directory).is_ok() {
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join("app.log"))
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

fn hide_child_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
}

fn safe_url(url: &Url) -> String {
    let mut safe = url.clone();
    safe.set_query(None);
    safe.set_fragment(None);
    safe.to_string()
}

fn query_summary(url: &Url) -> String {
    let Some(query) = url.query() else {
        return "<none>".into();
    };
    query
        .split('&')
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let key = urlencoding_key(key);
            if matches!(
                key.to_ascii_lowercase().as_str(),
                "sig" | "token" | "authorization" | "key"
            ) {
                format!("{key}=<redacted>")
            } else if value.is_empty() {
                format!("{key}=<empty>")
            } else {
                format!("{key}=<present>")
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn urlencoding_key(value: &str) -> String {
    value
        .replace("%3D", "=")
        .replace("%3d", "=")
        .replace('+', " ")
}

fn response_header(response: &reqwest::blocking::Response, name: &str) -> String {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("<missing>")
        .to_owned()
}

fn body_excerpt(body: &str) -> String {
    const MAX_CHARS: usize = 2_000;
    let excerpt: String = body
        .chars()
        .map(|character| {
            if character.is_control() && character != '\n' {
                ' '
            } else {
                character
            }
        })
        .take(MAX_CHARS)
        .collect();
    if body.chars().count() > MAX_CHARS {
        format!("{excerpt}… [truncated]")
    } else {
        excerpt
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadStatus {
    Queued,
    Downloading,
    Extracting,
    Registering,
    Paused,
    Completed,
    Cancelled,
    Failed(String),
}

pub struct Download {
    pub id: u64,
    pub name: String,
    pub path: PathBuf,
    pub received: u64,
    pub total: u64,
    pub speed: u64,
    pub status: DownloadStatus,
    pub downloaded_size: u64,
    pub registration: Option<Result<(), String>>,
    control: Arc<Control>,
    product_id: Option<String>,
    epic_id: Option<String>,
    restored: bool,
}

impl Download {
    /// Dossier d'installation du jeu (le paquet Xbox est téléchargé dans ce dossier).
    pub fn install_folder(&self) -> &Path {
        if self.product_id.is_some() {
            self.path.parent().unwrap_or(&self.path)
        } else {
            &self.path
        }
    }

    pub fn store(&self) -> crate::model::GameStore {
        if self.epic_id.is_some() {
            crate::model::GameStore::Epic
        } else {
            crate::model::GameStore::Xbox
        }
    }

    /// Identifiant du jeu dans son catalogue (product ID Xbox ou identifiant Epic).
    pub fn game_id(&self) -> Option<&str> {
        self.product_id.as_deref().or(self.epic_id.as_deref())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ControlState {
    Running,
    Paused,
    Cancelled,
}

struct Control {
    state: Mutex<ControlState>,
    changed: Condvar,
}

struct QueueState {
    waiting: VecDeque<u64>,
    active: usize,
}

struct DownloadQueue {
    state: Mutex<QueueState>,
    changed: Condvar,
}

struct QueuePermit(Arc<DownloadQueue>);

impl DownloadQueue {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(QueueState {
                waiting: VecDeque::new(),
                active: 0,
            }),
            changed: Condvar::new(),
        })
    }

    fn enqueue(&self, id: u64) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.waiting.contains(&id) {
            state.waiting.push_back(id);
        }
        self.changed.notify_all();
    }

    fn acquire(self: &Arc<Self>, id: u64, control: &Control) -> Option<QueuePermit> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if control.get() == ControlState::Cancelled {
                state.waiting.retain(|waiting| *waiting != id);
                self.changed.notify_all();
                return None;
            }
            if control.get() == ControlState::Paused {
                state.waiting.retain(|waiting| *waiting != id);
                self.changed.notify_all();
                drop(state);
                let _ = control.wait_until_resumed();
                state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                if control.get() != ControlState::Cancelled && !state.waiting.contains(&id) {
                    state.waiting.push_back(id);
                }
                self.changed.notify_all();
                continue;
            }
            if state.active < 1 && state.waiting.front() == Some(&id) {
                state.waiting.pop_front();
                state.active += 1;
                return Some(QueuePermit(Arc::clone(self)));
            }
            state = self
                .changed
                .wait_timeout(state, Duration::from_millis(200))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

impl Drop for QueuePermit {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.active = state.active.saturating_sub(1);
        self.0.changed.notify_all();
    }
}

impl Control {
    fn new() -> Self {
        Self {
            state: Mutex::new(ControlState::Running),
            changed: Condvar::new(),
        }
    }

    fn set(&self, state: ControlState) {
        let mut current = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        *current = state;
        self.changed.notify_all();
    }

    fn get(&self) -> ControlState {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait_until_resumed(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        while *state == ControlState::Paused {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        *state != ControlState::Cancelled
    }
}

enum DownloadEvent {
    Extracting {
        id: u64,
        package_bytes: u64,
    },
    ExtractionProgress {
        id: u64,
        percent: u64,
    },
    Registering {
        id: u64,
    },
    RegistrationResult {
        id: u64,
        result: Result<(), String>,
    },
    Progress {
        id: u64,
        received: u64,
        total: u64,
        speed: u64,
    },
    Finished {
        id: u64,
        result: Result<(), String>,
    },
}

pub struct DownloadManager {
    pub entries: Vec<Download>,
    pub notice: String,
    next_id: u64,
    tx: mpsc::Sender<DownloadEvent>,
    rx: Receiver<DownloadEvent>,
    /// Dernier contenu écrit dans le fichier d'état : évite de réécrire le disque à chaque
    /// événement de progression alors que la liste persistée n'a pas changé.
    last_persisted: Mutex<Vec<u8>>,
    queue: Arc<DownloadQueue>,
    library_needs_scan: bool,
}

impl DownloadManager {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let mut manager = Self {
            entries: Vec::new(),
            notice: String::new(),
            next_id: 1,
            tx,
            rx,
            last_persisted: Mutex::new(Vec::new()),
            queue: DownloadQueue::new(),
            library_needs_scan: false,
        };
        manager.restore_xbox_downloads();
        manager.resume_restored_downloads();
        manager
    }

    fn persist_downloads(&self) {
        let Some(path) = download_state_path() else {
            return;
        };
        let Some(parent) = path.parent() else { return };
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        let items: Vec<_> = self.entries.iter().filter_map(|entry| {
            if entry.product_id.is_none() && entry.epic_id.is_none() {
                return None;
            }
            if !matches!(entry.status, DownloadStatus::Queued | DownloadStatus::Downloading | DownloadStatus::Paused | DownloadStatus::Extracting | DownloadStatus::Registering) { return None; }
            Some(serde_json::json!({"id":entry.id,"product_id":entry.product_id,"epic_id":entry.epic_id,"name":entry.name,"path":entry.path}))
        }).collect();
        if let Ok(data) = serde_json::to_vec_pretty(&items) {
            let mut last = self
                .last_persisted
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if *last != data && fs::write(path, &data).is_ok() {
                *last = data;
            }
        }
    }

    fn restore_xbox_downloads(&mut self) {
        let Some(path) = download_state_path() else {
            return;
        };
        let Ok(data) = fs::read(path) else { return };
        let Ok(items) = serde_json::from_slice::<Vec<serde_json::Value>>(&data) else {
            return;
        };
        for item in &items {
            let (Some(id), Some(product_id), Some(name), Some(destination)) = (
                item.get("id").and_then(|v| v.as_u64()),
                item.get("product_id").and_then(|v| v.as_str()),
                item.get("name").and_then(|v| v.as_str()),
                item.get("path").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            if !valid_product_id(product_id) {
                continue;
            }
            let destination = PathBuf::from(destination);
            if !destination.is_absolute() {
                continue;
            }
            let partial = partial_path(&destination);
            if !partial.exists() && !destination.exists() {
                continue;
            }
            let has_complete_package = destination.is_file();
            let received = fs::metadata(&partial).map(|m| m.len()).unwrap_or(0);
            let control = Arc::new(Control::new());
            control.set(ControlState::Paused);
            self.next_id = self.next_id.max(id.saturating_add(1));
            self.entries.push(Download {
                id,
                name: name.to_owned(),
                path: destination.clone(),
                received,
                total: 0,
                speed: 0,
                status: DownloadStatus::Paused,
                downloaded_size: if has_complete_package {
                    fs::metadata(&destination).map(|m| m.len()).unwrap_or(0)
                } else {
                    received
                },
                registration: None,
                control,
                product_id: Some(product_id.to_owned()),
                epic_id: None,
                restored: true,
            });
        }
        for item in &items {
            let (Some(id), Some(epic_id), Some(name), Some(root)) = (
                item.get("id").and_then(|v| v.as_u64()),
                item.get("epic_id").and_then(|v| v.as_str()),
                item.get("name").and_then(|v| v.as_str()),
                item.get("path").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            let root = PathBuf::from(root);
            if !root.is_absolute() || !root.is_dir() {
                continue;
            }
            let control = Arc::new(Control::new());
            control.set(ControlState::Paused);
            self.next_id = self.next_id.max(id.saturating_add(1));
            self.entries.push(Download {
                id,
                name: name.to_owned(),
                path: root,
                received: 0,
                total: 0,
                speed: 0,
                status: DownloadStatus::Paused,
                downloaded_size: 0,
                registration: None,
                control,
                product_id: None,
                epic_id: Some(epic_id.to_owned()),
                restored: true,
            });
        }
    }

    fn resume_restored_downloads(&mut self) {
        let ids: Vec<u64> = self
            .entries
            .iter()
            .filter(|entry| entry.restored && entry.product_id.is_some())
            .map(|entry| entry.id)
            .collect();
        for id in ids {
            self.resume(id);
        }
    }

    pub fn start_xbox(
        &mut self,
        product_id: &str,
        game_name: &str,
        folder: &str,
    ) -> Result<(), String> {
        if !valid_product_id(product_id) {
            return Err("Invalid Xbox product ID.".into());
        }
        let folder = PathBuf::from(folder.trim());
        if !folder.is_absolute() {
            return Err("Choose an absolute download folder.".into());
        }
        let game_dir = folder.join(sanitize_filename(game_name));
        fs::create_dir(&game_dir)
            .map_err(|e| format!("Could not create the game folder (it may already exist): {e}"))?;
        let destination = game_dir.join("package.msixvc");
        if destination.exists() {
            return Err(format!(
                "The package already exists: {}",
                destination.display()
            ));
        }
        let partial_path = partial_path(&destination);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial_path)
            .map_err(|e| format!("Could not create the package file: {e}"))?;
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| "No more download IDs are available.".to_owned())?;
        let control = Arc::new(Control::new());
        self.entries.push(Download {
            id,
            name: game_name.to_owned(),
            path: destination.clone(),
            received: 0,
            total: 0,
            speed: 0,
            status: DownloadStatus::Queued,
            downloaded_size: 0,
            registration: None,
            control: Arc::clone(&control),
            product_id: Some(product_id.to_owned()),
            epic_id: None,
            restored: false,
        });
        self.persist_downloads();
        self.notice.clear();
        let tx = self.tx.clone();
        let queue = Arc::clone(&self.queue);
        queue.enqueue(id);
        let product_id = product_id.to_owned();
        let game_dir = destination.parent().unwrap().to_path_buf();
        download_log(format!(
            "Xbox download queued: product_id={product_id}, title={game_name}, output={}",
            game_dir.display()
        ));
        std::thread::spawn(move || {
            if let Some(_permit) = queue.acquire(id, &control) {
                run_xbox_download(
                    id,
                    product_id,
                    destination,
                    game_dir,
                    partial_path,
                    control,
                    tx,
                );
            } else {
                let _ = remove_partial(&partial_path);
                let _ = fs::remove_file(&destination);
                let _ = fs::remove_dir_all(&game_dir);
                let _ = tx.send(DownloadEvent::Finished {
                    id,
                    result: Err("cancelled".into()),
                });
            }
        });
        Ok(())
    }

    /// Installe un build Epic à partir du manifest BuildPatch et de ses CDN publics.
    pub fn start_epic(&mut self, game: &Game, folder: &str) -> Result<(), String> {
        if game.manifest_url.is_empty() || game.cdn_bases.is_empty() {
            return Err("Epic API is missing the manifest or CDN metadata for this game.".into());
        }
        let folder = PathBuf::from(folder.trim());
        if !folder.is_absolute() {
            return Err("Choose an absolute download folder.".into());
        }
        fs::create_dir_all(&folder).map_err(|e| format!("Could not create library folder: {e}"))?;
        let game_dir = folder.join(sanitize_filename(&game.name));
        fs::create_dir(&game_dir)
            .map_err(|e| format!("Could not create game folder (it may already exist): {e}"))?;
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| "No more download IDs are available.".to_owned())?;
        let control = Arc::new(Control::new());
        self.entries.push(Download {
            id,
            name: game.name.clone(),
            path: game_dir.clone(),
            received: 0,
            total: 0,
            speed: 0,
            status: DownloadStatus::Queued,
            downloaded_size: 0,
            registration: None,
            control: Arc::clone(&control),
            product_id: None,
            epic_id: Some(game.id.clone()),
            restored: false,
        });
        self.notice.clear();
        let tx = self.tx.clone();
        let queue = Arc::clone(&self.queue);
        queue.enqueue(id);
        let game = game.clone();
        std::thread::spawn(move || {
            let result = if let Some(_permit) = queue.acquire(id, &control) {
                run_epic_download(id, game, game_dir.clone(), control, tx.clone())
            } else {
                Err("cancelled".into())
            };
            if result.is_err() {
                let _ = fs::remove_dir_all(&game_dir);
            }
            let _ = tx.send(DownloadEvent::Finished { id, result });
        });
        Ok(())
    }

    pub fn pause(&mut self, id: u64) {
        if let Some(entry) = self.entries.iter_mut().find(|item| item.id == id) {
            if matches!(
                entry.status,
                DownloadStatus::Downloading
                    | DownloadStatus::Queued
                    | DownloadStatus::Extracting
                    | DownloadStatus::Registering
            ) {
                entry.control.set(ControlState::Paused);
                entry.status = DownloadStatus::Paused;
            }
        }
        self.persist_downloads();
    }

    pub fn resume(&mut self, id: u64) {
        let restored_index = self.entries.iter().position(|item| {
            item.id == id && item.status == DownloadStatus::Paused && item.restored
        });
        if let Some(index) = restored_index {
            let entry = &mut self.entries[index];
            let Some(product_id) = entry.product_id.clone() else {
                return;
            };
            let destination = entry.path.clone();
            let Some(game_dir) = destination.parent().map(Path::to_path_buf) else {
                return;
            };
            let partial = partial_path(&destination);
            let control = Arc::clone(&entry.control);
            entry.restored = false;
            entry.control.set(ControlState::Running);
            entry.status = DownloadStatus::Queued;
            let tx = self.tx.clone();
            let id = entry.id;
            let queue = Arc::clone(&self.queue);
            queue.enqueue(id);
            std::thread::spawn(move || {
                if let Some(_permit) = queue.acquire(id, &control) {
                    run_xbox_download(id, product_id, destination, game_dir, partial, control, tx);
                } else {
                    let _ = remove_partial(&partial);
                    let _ = fs::remove_file(&destination);
                    let _ = fs::remove_dir_all(&game_dir);
                    let _ = tx.send(DownloadEvent::Finished {
                        id,
                        result: Err("cancelled".into()),
                    });
                }
            });
            self.persist_downloads();
            return;
        }
        if let Some(entry) = self.entries.iter_mut().find(|item| item.id == id) {
            if entry.status == DownloadStatus::Paused {
                entry.control.set(ControlState::Running);
                entry.status = DownloadStatus::Downloading;
            }
        }
        self.persist_downloads();
    }

    pub fn resume_restored_epic(&mut self, games: &[Game]) {
        let resumable: Vec<(u64, Game)> = self
            .entries
            .iter()
            .filter(|entry| entry.restored && entry.epic_id.is_some())
            .filter_map(|entry| {
                let game = games
                    .iter()
                    .find(|game| Some(&game.id) == entry.epic_id.as_ref())?;
                Some((entry.id, game.clone()))
            })
            .collect();
        for (id, game) in resumable {
            let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) else {
                continue;
            };
            entry.restored = false;
            entry.control.set(ControlState::Running);
            entry.status = DownloadStatus::Queued;
            let control = Arc::clone(&entry.control);
            let root = entry.path.clone();
            let tx = self.tx.clone();
            let queue = Arc::clone(&self.queue);
            queue.enqueue(id);
            std::thread::spawn(move || {
                let result = if let Some(_permit) = queue.acquire(id, &control) {
                    run_epic_download(id, game, root.clone(), control, tx.clone())
                } else {
                    Err("cancelled".into())
                };
                if result.is_err() {
                    let _ = fs::remove_dir_all(&root);
                }
                let _ = tx.send(DownloadEvent::Finished { id, result });
            });
        }
        self.persist_downloads();
    }

    pub fn cancel(&mut self, id: u64) {
        if let Some(entry) = self.entries.iter_mut().find(|item| item.id == id) {
            if matches!(
                entry.status,
                DownloadStatus::Queued
                    | DownloadStatus::Downloading
                    | DownloadStatus::Paused
                    | DownloadStatus::Extracting
                    | DownloadStatus::Registering
            ) {
                entry.control.set(ControlState::Cancelled);
                entry.status = DownloadStatus::Cancelled;
            }
        }
        self.persist_downloads();
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.rx.try_recv() {
                Ok(DownloadEvent::Progress {
                    id,
                    received,
                    total,
                    speed,
                }) => {
                    if let Some(entry) = self.entries.iter_mut().find(|item| item.id == id) {
                        entry.received = received;
                        entry.total = total;
                        entry.speed = speed;
                        entry.status = match entry.control.get() {
                            ControlState::Running
                                if matches!(
                                    entry.status,
                                    DownloadStatus::Extracting | DownloadStatus::Registering
                                ) =>
                            {
                                entry.status.clone()
                            }
                            ControlState::Running => DownloadStatus::Downloading,
                            ControlState::Paused => DownloadStatus::Paused,
                            ControlState::Cancelled => DownloadStatus::Cancelled,
                        };
                        changed = true;
                    }
                }
                Ok(DownloadEvent::Finished { id, result }) => {
                    if let Some(entry) = self.entries.iter_mut().find(|item| item.id == id) {
                        entry.speed = 0;
                        if matches!(result, Err(ref error) if error.starts_with("cancelled")) {
                            self.library_needs_scan = true;
                        }
                        entry.status = match result {
                            Ok(()) => {
                                entry.received = entry.total.max(entry.received);
                                DownloadStatus::Completed
                            }
                            Err(error) if error == "cancelled" => DownloadStatus::Cancelled,
                            Err(error) if error == "paused" => DownloadStatus::Paused,
                            Err(error) => DownloadStatus::Failed(error),
                        };
                        changed = true;
                    }
                }
                Ok(DownloadEvent::Extracting { id, package_bytes }) => {
                    if let Some(entry) = self.entries.iter_mut().find(|item| item.id == id) {
                        entry.speed = 0;
                        entry.received = 0;
                        entry.total = 100;
                        entry.downloaded_size = package_bytes;
                        entry.status = match entry.control.get() {
                            ControlState::Running => DownloadStatus::Extracting,
                            ControlState::Paused => DownloadStatus::Paused,
                            ControlState::Cancelled => DownloadStatus::Cancelled,
                        };
                        changed = true;
                    }
                }
                Ok(DownloadEvent::Registering { id }) => {
                    if let Some(entry) = self.entries.iter_mut().find(|item| item.id == id) {
                        entry.speed = 0;
                        entry.received = 0;
                        entry.total = 0;
                        entry.status = match entry.control.get() {
                            ControlState::Running => DownloadStatus::Registering,
                            ControlState::Paused => DownloadStatus::Paused,
                            ControlState::Cancelled => DownloadStatus::Cancelled,
                        };
                        changed = true;
                    }
                }
                Ok(DownloadEvent::RegistrationResult { id, result }) => {
                    if let Some(entry) = self.entries.iter_mut().find(|item| item.id == id) {
                        entry.registration = Some(result);
                        changed = true;
                    }
                }
                Ok(DownloadEvent::ExtractionProgress { id, percent }) => {
                    if let Some(entry) = self.entries.iter_mut().find(|item| item.id == id) {
                        entry.received = percent.min(100);
                        entry.total = 100;
                        entry.status = match entry.control.get() {
                            ControlState::Running => DownloadStatus::Extracting,
                            ControlState::Paused => DownloadStatus::Paused,
                            ControlState::Cancelled => DownloadStatus::Cancelled,
                        };
                        changed = true;
                    }
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if changed {
            self.persist_downloads();
        }
        changed |= self.prune_missing_completed();
        changed
    }

    fn prune_missing_completed(&mut self) -> bool {
        let count = self.entries.len();
        self.entries.retain(|entry| {
            if entry.status != DownloadStatus::Completed {
                return true;
            }
            let installed_path = if entry.product_id.is_some() {
                entry.path.parent().unwrap_or(&entry.path)
            } else {
                &entry.path
            };
            installed_path.exists()
        });
        self.entries.len() != count
    }

    pub fn is_active(&self) -> bool {
        self.entries.iter().any(|entry| {
            matches!(
                entry.status,
                DownloadStatus::Queued
                    | DownloadStatus::Downloading
                    | DownloadStatus::Paused
                    | DownloadStatus::Extracting
                    | DownloadStatus::Registering
            )
        })
    }

    pub fn take_library_change(&mut self) -> bool {
        std::mem::take(&mut self.library_needs_scan)
    }

    pub fn incomplete_library_paths(&self) -> Vec<PathBuf> {
        self.entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry.status,
                    DownloadStatus::Queued
                        | DownloadStatus::Downloading
                        | DownloadStatus::Extracting
                        | DownloadStatus::Registering
                        | DownloadStatus::Paused
                        | DownloadStatus::Cancelled
                        | DownloadStatus::Failed(_)
                )
            })
            .map(|entry| {
                if entry.product_id.is_some() {
                    entry.path.parent().unwrap_or(&entry.path).to_path_buf()
                } else {
                    entry.path.clone()
                }
            })
            .collect()
    }
}

#[derive(Clone)]
struct EpicChunk {
    guid: [u32; 4],
    hash: u64,
    group: u8,
    size: u64,
}
#[derive(Clone)]
struct EpicPart {
    guid: [u32; 4],
    offset: u32,
    size: u32,
}
struct EpicFile {
    name: String,
    sha1: Vec<u8>,
    parts: Vec<EpicPart>,
}
struct EpicChunkCache(PathBuf);
impl Drop for EpicChunkCache {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Débit moyen sur une fenêtre glissante : le téléchargement Epic arrive par chunks, donc on
/// ne peut pas se contenter d'un échantillon instantané. Seuls les octets réellement
/// téléchargés comptent (pas les chunks repris depuis le cache).
struct SpeedMeter {
    samples: VecDeque<(Instant, u64)>,
    total: u64,
}

impl SpeedMeter {
    const WINDOW: Duration = Duration::from_secs(6);

    fn new() -> Self {
        Self {
            samples: VecDeque::from([(Instant::now(), 0)]),
            total: 0,
        }
    }

    fn add(&mut self, bytes: u64) {
        self.total = self.total.saturating_add(bytes);
        self.samples.push_back((Instant::now(), self.total));
        while self.samples.len() > 1 && self.samples[1].0.elapsed() > Self::WINDOW {
            self.samples.pop_front();
        }
    }

    fn speed(&self) -> u64 {
        let Some(&(start, bytes_at_start)) = self.samples.front() else {
            return 0;
        };
        let elapsed = start.elapsed().as_secs_f64();
        if elapsed < 0.25 || self.total == bytes_at_start {
            return 0;
        }
        (self.total.saturating_sub(bytes_at_start) as f64 / elapsed) as u64
    }
}

fn run_epic_download(
    id: u64,
    game: Game,
    root: PathBuf,
    control: Arc<Control>,
    tx: mpsc::Sender<DownloadEvent>,
) -> Result<(), String> {
    use sha1::Digest;
    let client = Client::builder()
        .timeout(Duration::from_secs(180))
        .user_agent(crate::net::USER_AGENT)
        .build()
        .map_err(|e| e.to_string())?;
    let manifest_url = if game.manifest_url.starts_with("http") {
        game.manifest_url.clone()
    } else {
        format!("{}{}", crate::catalog::EPIC_API_BASE, game.manifest_url)
    };
    let bytes = client
        .get(&manifest_url)
        .send()
        .map_err(|e| format!("Epic manifest request failed: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Epic manifest request failed: {e}"))?
        .bytes()
        .map_err(|e| e.to_string())?;
    let (chunks, files, feature) = parse_epic_manifest(&bytes)?;
    let by_guid: std::collections::HashMap<_, _> =
        chunks.into_iter().map(|c| (c.guid, c)).collect();
    let bytes_total: u64 = by_guid.values().map(|c| c.size).sum();
    if bytes_total == 0 {
        return Err("Epic manifest contains no chunks.".into());
    }
    let mut meter = SpeedMeter::new();
    let cache_dir = root.join(".ud-epic-chunks");
    fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("Could not create Epic chunk cache: {e}"))?;
    let _cache_guard = EpicChunkCache(cache_dir.clone());
    let mut cached = std::collections::HashMap::<[u32; 4], PathBuf>::new();
    let mut pending = VecDeque::new();
    let mut received_chunks = 0u64;
    for chunk in by_guid.values() {
        let cache_path = cache_dir.join(format!(
            "{:08X}{:08X}{:08X}{:08X}.chunk",
            chunk.guid[0], chunk.guid[1], chunk.guid[2], chunk.guid[3]
        ));
        let reusable = fs::read(&cache_path)
            .ok()
            .filter(|raw| epic_chunk_data(raw).is_ok());
        if reusable.is_some() {
            cached.insert(chunk.guid, cache_path);
            received_chunks = received_chunks.saturating_add(chunk.size);
        } else {
            pending.push_back(chunk.clone());
        }
    }
    if received_chunks > 0 {
        publish_progress(&tx, id, received_chunks, bytes_total, 0);
    }

    // Epic manifests can contain thousands of small chunks. Fetching them one at a time
    // leaves the connection idle between requests, so use a small bounded worker pool.
    const EPIC_CHUNK_WORKERS: usize = 8;
    let pending_count = pending.len();
    if pending_count > 0 {
        let jobs = Arc::new(Mutex::new(pending));
        let (result_tx, result_rx) = mpsc::channel();
        let worker_count = EPIC_CHUNK_WORKERS.min(pending_count);
        std::thread::scope(|scope| -> Result<(), String> {
            for _ in 0..worker_count {
                let jobs = Arc::clone(&jobs);
                let client = client.clone();
                let result_tx = result_tx.clone();
                let cdns = &game.cdn_bases;
                let cache_dir = &cache_dir;
                let control = &control;
                scope.spawn(move || {
                    loop {
                        if !control.wait_until_resumed() {
                            break;
                        }
                        let chunk = jobs
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .pop_front();
                        let Some(chunk) = chunk else { break };
                        let path = epic_chunk_path(&chunk, feature);
                        let cache_path = cache_dir.join(format!(
                            "{:08X}{:08X}{:08X}{:08X}.chunk",
                            chunk.guid[0], chunk.guid[1], chunk.guid[2], chunk.guid[3]
                        ));
                        let fetched = (|| -> Result<(PathBuf, u64), String> {
                            let mut last_error = None;
                            for base in cdns {
                                if !control.wait_until_resumed() {
                                    return Err("cancelled".into());
                                }
                                let url = format!("{}/{}", base.trim_end_matches('/'), path);
                                match client
                                    .get(&url)
                                    .send()
                                    .and_then(|response| response.error_for_status())
                                    .and_then(|response| response.bytes())
                                {
                                    Ok(data) => {
                                        let raw = data.to_vec();
                                        if let Err(error) = epic_chunk_data(&raw) {
                                            last_error = Some(error);
                                            continue;
                                        }
                                        fs::write(&cache_path, &raw).map_err(|error| {
                                            format!("Could not cache Epic chunk: {error}")
                                        })?;
                                        return Ok((cache_path, raw.len() as u64));
                                    }
                                    Err(error) => {
                                        last_error = Some(error.without_url().to_string())
                                    }
                                }
                            }
                            Err(format!(
                                "Could not fetch Epic chunk {path} from available CDNs{}.",
                                last_error
                                    .map(|error| format!(" ({error})"))
                                    .unwrap_or_default()
                            ))
                        })();
                        if result_tx.send((chunk.guid, chunk.size, fetched)).is_err() {
                            break;
                        }
                    }
                });
            }
            drop(result_tx);
            let mut first_error = None;
            for _ in 0..pending_count {
                let (guid, size, result) = result_rx
                    .recv()
                    .map_err(|_| "Epic chunk workers stopped unexpectedly.".to_owned())?;
                match result {
                    Ok((cache_path, downloaded_bytes)) => {
                        cached.insert(guid, cache_path);
                        received_chunks = received_chunks.saturating_add(size);
                        meter.add(downloaded_bytes);
                        publish_progress(&tx, id, received_chunks, bytes_total, meter.speed());
                    }
                    Err(error) => {
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                }
            }
            if let Some(error) = first_error {
                Err(error)
            } else {
                Ok(())
            }
        })?;
    }

    for file in files {
        if !control.wait_until_resumed() {
            return Err("cancelled".into());
        }
        if file.parts.is_empty() {
            let relative = Path::new(&file.name);
            if relative.is_absolute()
                || relative.components().any(|c| {
                    matches!(
                        c,
                        std::path::Component::ParentDir | std::path::Component::Prefix(_)
                    )
                })
            {
                return Err(format!("Unsafe path in Epic manifest: {}", file.name));
            }
            let destination = root.join(relative);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            File::create(&destination)
                .map_err(|e| format!("Could not create {}: {e}", destination.display()))?;
            if sha1::Sha1::digest([]).as_slice() != file.sha1.as_slice() {
                return Err(format!(
                    "Unsupported Epic manifest entry without chunk data: {}",
                    file.name
                ));
            }
            continue;
        }
        let relative = Path::new(&file.name);
        if relative.is_absolute()
            || relative.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir | std::path::Component::Prefix(_)
                )
            })
        {
            return Err(format!("Unsafe path in Epic manifest: {}", file.name));
        }
        let destination = root.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut output = File::create(&destination)
            .map_err(|e| format!("Could not create {}: {e}", destination.display()))?;
        let mut file_hash = sha1::Sha1::new();
        for part in file.parts {
            if !control.wait_until_resumed() {
                return Err("cancelled".into());
            }
            if !by_guid.contains_key(&part.guid) {
                return Err(format!(
                    "Epic manifest references an unknown chunk for {}",
                    file.name
                ));
            }
            let raw = fs::read(cached.get(&part.guid).unwrap())
                .map_err(|e| format!("Could not read cached Epic chunk: {e}"))?;
            let data = epic_chunk_data(&raw)?;
            let start = part.offset as usize;
            let end = start
                .checked_add(part.size as usize)
                .filter(|end| *end <= data.len())
                .ok_or("Epic chunk part is out of range.")?;
            output
                .write_all(&data[start..end])
                .map_err(|e| e.to_string())?;
            file_hash.update(&data[start..end]);
        }
        output.flush().map_err(|e| e.to_string())?;
        if file_hash.finalize().as_slice() != file.sha1.as_slice() {
            return Err(format!("File hash verification failed: {}", file.name));
        }
    }
    Ok(())
}

fn parse_epic_manifest(raw: &[u8]) -> Result<(Vec<EpicChunk>, Vec<EpicFile>, u32), String> {
    use sha1::Digest;
    use std::io::Read;
    let mut r = EpicReader { b: raw, p: 0 };
    if r.u32()? != 0x44BEC00C {
        return Err("Epic manifest has an invalid BuildPatch header.".into());
    }
    let header = r.u32()? as usize;
    let unpacked = r.u32()? as usize;
    let _compressed = r.u32()? as usize;
    let sha = r.take(20)?.to_vec();
    let stored = r.u8()?;
    let feature = r.u32()?;
    if feature >= 22 {
        r.take(32)?;
    }
    r.p = header;
    let body = if stored & 1 != 0 {
        let mut z = flate2::read::ZlibDecoder::new(&raw[r.p..]);
        let mut out = Vec::with_capacity(unpacked);
        z.read_to_end(&mut out)
            .map_err(|e| format!("Could not decompress Epic manifest: {e}"))?;
        out
    } else {
        raw[r.p..].to_vec()
    };
    if stored & 1 != 0 && sha1::Sha1::digest(&body).as_slice() != sha.as_slice() {
        return Err("Epic manifest SHA-1 validation failed.".into());
    }
    let mut r = EpicReader { b: &body, p: 0 };
    let meta_size = r.u32()? as usize;
    r.u8()?;
    r.u32()?;
    r.u8()?;
    r.u32()?;
    r.string()?;
    r.string()?;
    r.string()?;
    r.string()?;
    let prereq = r.u32()?;
    for _ in 0..prereq {
        r.string()?;
    }
    r.string()?;
    r.string()?;
    r.string()?;
    r.p = meta_size;
    let cdl_start = r.p;
    let cdl_size = r.u32()? as usize;
    r.u8()?;
    let count = r.u32()? as usize;
    if count > 2_000_000 {
        return Err("Epic manifest chunk count is invalid.".into());
    }
    let mut guids = Vec::with_capacity(count);
    for _ in 0..count {
        guids.push(r.guid()?);
    }
    let mut hashes = Vec::with_capacity(count);
    for _ in 0..count {
        hashes.push(r.u64()?);
    }
    r.take(count * 20)?;
    let mut groups = Vec::with_capacity(count);
    for _ in 0..count {
        groups.push(r.u8()?);
    }
    r.take(count * 4)?;
    let mut sizes = Vec::with_capacity(count);
    for _ in 0..count {
        let n = i64::from_le_bytes(r.take(8)?.try_into().unwrap());
        sizes.push(n.max(0) as u64);
    }
    if feature >= 22 {
        r.take(count * 36)?;
    }
    r.p = cdl_start + cdl_size;
    let chunks = guids
        .into_iter()
        .zip(hashes)
        .zip(groups)
        .zip(sizes)
        .map(|(((guid, hash), group), size)| EpicChunk {
            guid,
            hash,
            group,
            size,
        })
        .collect();
    let fml_start = r.p;
    let fml_size = r.u32()? as usize;
    let fml_ver = r.u8()?;
    let nf = r.u32()? as usize;
    if nf > 2_000_000 {
        return Err("Epic manifest file count is invalid.".into());
    }
    let mut names = Vec::with_capacity(nf);
    for _ in 0..nf {
        names.push(r.string()?);
    }
    for _ in 0..nf {
        r.string()?;
    }
    let mut hashes = Vec::with_capacity(nf);
    for _ in 0..nf {
        hashes.push(r.take(20)?.to_vec());
    }
    r.take(nf)?;
    for _ in 0..nf {
        let tags = r.u32()?;
        for _ in 0..tags {
            r.string()?;
        }
    }
    let mut files = Vec::with_capacity(nf);
    for i in 0..nf {
        let np = r.u32()? as usize;
        if np > 2_000_000 {
            return Err("Epic file chunk count is invalid.".into());
        }
        let mut parts = Vec::with_capacity(np);
        for _ in 0..np {
            let size = r.u32()?;
            let guid = r.guid()?;
            let offset = r.u32()?;
            let n = r.u32()?;
            if size > 28 {
                r.take((size - 28) as usize)?;
            }
            parts.push(EpicPart {
                guid,
                offset,
                size: n,
            });
        }
        files.push(EpicFile {
            name: names[i].clone(),
            sha1: hashes[i].clone(),
            parts,
        });
    }
    if fml_ver >= 1 {
        for _ in 0..nf {
            if r.u32()? != 0 {
                r.take(16)?;
            }
        }
        for _ in 0..nf {
            r.string()?;
        }
    }
    if fml_ver >= 2 {
        r.take(nf * 32)?;
    }
    let fml_end = fml_start
        .checked_add(fml_size)
        .ok_or("Epic manifest FML size overflowed.")?;
    if r.p > fml_end {
        return Err("Epic manifest FML section exceeded its declared size.".into());
    }
    Ok((chunks, files, feature))
}

struct EpicReader<'a> {
    b: &'a [u8],
    p: usize,
}
impl<'a> EpicReader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let e = self
            .p
            .checked_add(n)
            .filter(|e| *e <= self.b.len())
            .ok_or("Unexpected end of Epic manifest.")?;
        let s = &self.b[self.p..e];
        self.p = e;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn guid(&mut self) -> Result<[u32; 4], String> {
        Ok([self.u32()?, self.u32()?, self.u32()?, self.u32()?])
    }
    fn string(&mut self) -> Result<String, String> {
        let n = self.u32()? as i32;
        if n == 0 {
            return Ok(String::new());
        }
        if n > 0 {
            let b = self.take(n as usize)?;
            return Ok(String::from_utf8_lossy(&b[..b.len() - 1]).into_owned());
        }
        let count = (-n) as usize;
        let b = self.take(count * 2)?;
        String::from_utf16(
            &b.chunks_exact(2)
                .take(count.saturating_sub(1))
                .map(|x| u16::from_le_bytes([x[0], x[1]]))
                .collect::<Vec<_>>(),
        )
        .map_err(|e| e.to_string())
    }
}

fn epic_chunk_path(chunk: &EpicChunk, version: u32) -> String {
    let dir = if version >= 22 {
        "ChunksV5"
    } else if version >= 15 {
        "ChunksV4"
    } else if version >= 6 {
        "ChunksV3"
    } else if version >= 3 {
        "ChunksV2"
    } else {
        "Chunks"
    };
    format!(
        "{dir}/{:02}/{:016X}_{}.chunk",
        chunk.group,
        chunk.hash,
        chunk
            .guid
            .iter()
            .map(|x| format!("{x:08X}"))
            .collect::<String>()
    )
}
fn epic_chunk_data(raw: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut r = EpicReader { b: raw, p: 0 };
    if r.u32()? != 0xB1FE3AA2 {
        return Err("Epic chunk has an invalid header.".into());
    }
    let version = r.u32()?;
    let header = r.u32()? as usize;
    let size = r.u32()? as usize;
    r.take(16 + 8)?;
    let stored = r.u8()?;
    if version >= 2 {
        r.take(21)?;
    }
    if version >= 3 {
        r.u32()?;
    }
    if version >= 4 {
        return Err(
            "Encrypted Epic chunks (header v4+) require a key not provided by EpicApi.".into(),
        );
    }
    r.p = header;
    let payload = r.take(size)?;
    if stored & 1 != 0 {
        let mut z = flate2::read::ZlibDecoder::new(payload);
        let mut out = Vec::new();
        z.read_to_end(&mut out).map_err(|e| e.to_string())?;
        Ok(out)
    } else {
        Ok(payload.to_vec())
    }
}

fn run_xbox_download(
    id: u64,
    product_id: String,
    destination: PathBuf,
    game_dir: PathBuf,
    partial_path: PathBuf,
    control: Arc<Control>,
    tx: mpsc::Sender<DownloadEvent>,
) {
    let result = (|| {
        let expected_size = if destination.is_file() {
            0
        } else {
            let (url, expected_size) = resolve_xbox_package(&product_id)?;
            if !transfer_xbox_ranges(
                &url,
                expected_size,
                &destination,
                &partial_path,
                id,
                &control,
                &tx,
            )? {
                return Ok(false);
            }
            expected_size
        };
        if expected_size > 0
            && fs::metadata(&destination).map(|m| m.len()).unwrap_or(0) != expected_size
        {
            return Err(format!(
                "Downloaded package size differs from Microsoft metadata ({expected_size} bytes)."
            ));
        }
        let package_bytes = fs::metadata(&destination).map(|m| m.len()).unwrap_or(0);
        let _ = tx.send(DownloadEvent::Extracting { id, package_bytes });
        match extract_msixvc(
            &destination,
            &game_dir,
            &product_id,
            package_bytes,
            &control,
            &tx,
            id,
        )? {
            true => {
                let registered = register_extracted_package(&game_dir, &control, &tx, id);
                if matches!(registered, Ok(true)) {
                    let _ = fs::remove_file(&destination);
                }
                registered
            }
            false => Ok(false),
        }
    })();
    if let Err(error) = &result {
        download_log(format!(
            "Xbox download failed for product_id={product_id}: {error}"
        ));
    } else {
        download_log(format!(
            "Xbox download completed for product_id={product_id}"
        ));
    }
    let result = match result {
        Ok(true) => Ok(()),
        Ok(false) => {
            let partial_cleanup = remove_partial(&partial_path);
            let _ = fs::remove_file(&destination);
            let _ = fs::remove_dir_all(&game_dir);
            match partial_cleanup {
                Ok(()) => Err("cancelled".to_owned()),
                Err(error) => Err(format!(
                    "cancelled; could not remove partial package: {error}"
                )),
            }
        }
        Err(_error) if control.get() == ControlState::Paused => {
            download_log(format!(
                "Xbox task {id} interrupted while paused; keeping partial data"
            ));
            Err("paused".to_owned())
        }
        Err(error) => {
            if (error.starts_with("Could not read the download")
                || error.starts_with("Download request failed:")
                || error.starts_with("The connection ended early"))
                && ((partial_path.is_file()
                    && fs::metadata(&partial_path)
                        .map(|metadata| metadata.len())
                        .unwrap_or(0)
                        > 0)
                    || has_xbox_range_data(&partial_path))
            {
                download_log(format!(
                    "Keeping partial Xbox package for resume: {} bytes at {}",
                    fs::metadata(&partial_path)
                        .map(|metadata| metadata.len())
                        .unwrap_or(0),
                    partial_path.display()
                ));
                Err(error)
            } else {
                let cleanup = remove_partial(&partial_path);
                let _ = fs::remove_file(&destination);
                let _ = fs::remove_dir_all(&game_dir);
                Err(match cleanup {
                    Ok(()) => error,
                    Err(cleanup_error) => {
                        format!("{error}; could not remove partial file: {cleanup_error}")
                    }
                })
            }
        }
    };
    let _ = tx.send(DownloadEvent::Finished { id, result });
}

fn download_state_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|root| root.join("UniversalDownloader").join("downloads.json"))
}

fn resolve_asset_dir(folder: &str, override_key: &str, legacy_relative: &str) -> PathBuf {
    if let Some(path) = std::env::var_os(override_key) {
        return PathBuf::from(path);
    }
    let folder = Path::new(folder);
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        let beside_executable = directory.join(folder);
        if beside_executable.is_dir() {
            return beside_executable;
        }
    }
    let project_copy = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(folder);
    if project_copy.is_dir() {
        return project_copy;
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(legacy_relative)
}

fn read_developer_mode() -> Option<u32> {
    let mut command = Command::new("reg.exe");
    command.args([
        "query",
        r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock",
        "/v",
        "AllowDevelopmentWithoutDevLicense",
    ]);
    hide_child_console(&mut command);
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let value = text.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let _ = parts.next()?;
        let _ = parts.next()?;
        let raw = parts.next()?;
        raw.strip_prefix("0x")
            .and_then(|hex| u32::from_str_radix(hex, 16).ok())
            .or_else(|| raw.parse().ok())
    });
    value
}

fn write_developer_mode(value: Option<u32>) -> Result<(), String> {
    const KEY: &str = r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock";
    let mut args = vec![
        "add".to_owned(),
        KEY.to_owned(),
        "/v".into(),
        "AllowDevelopmentWithoutDevLicense".into(),
        "/t".into(),
        "REG_DWORD".into(),
        "/f".into(),
    ];
    if let Some(value) = value {
        args.splice(6..6, ["/d".to_owned(), value.to_string()]);
    } else {
        args[0] = "delete".into();
        args.retain(|arg| !matches!(arg.as_str(), "/t" | "REG_DWORD"));
    }
    let mut direct_command = Command::new("reg.exe");
    direct_command.args(&args);
    hide_child_console(&mut direct_command);
    let direct = direct_command
        .output()
        .map_err(|e| format!("Could not run reg.exe: {e}"))?;
    if direct.status.success() {
        return Ok(());
    }
    if value.is_none() && read_developer_mode().is_none() {
        return Ok(());
    }
    let powershell_args = args
        .iter()
        .map(|arg| format!("'{}'", arg.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    let script = format!(
        "$p = Start-Process -FilePath 'reg.exe' -ArgumentList @({powershell_args}) -Verb RunAs -Wait -PassThru; exit $p.ExitCode"
    );
    let mut elevated_command = Command::new("powershell.exe");
    elevated_command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    hide_child_console(&mut elevated_command);
    let elevated = elevated_command
        .status()
        .map_err(|e| format!("Could not request elevated registry access: {e}"))?;
    if elevated.success() {
        Ok(())
    } else {
        Err("Registry update was denied or failed.".into())
    }
}

#[cfg(windows)]
fn suspend_process(process_id: u32, suspend: bool) -> Result<(), String> {
    use std::ffi::c_void;

    #[repr(C)]
    struct ThreadEntry32 {
        size: u32,
        usage: u32,
        thread_id: u32,
        owner_process_id: u32,
        base_priority: i32,
        delta_priority: i32,
        flags: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> *mut c_void;
        fn Thread32First(snapshot: *mut c_void, entry: *mut ThreadEntry32) -> i32;
        fn Thread32Next(snapshot: *mut c_void, entry: *mut ThreadEntry32) -> i32;
        fn OpenThread(access: u32, inherit: i32, thread_id: u32) -> *mut c_void;
        fn SuspendThread(thread: *mut c_void) -> u32;
        fn ResumeThread(thread: *mut c_void) -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    const SNAP_THREADS: u32 = 0x0000_0004;
    const THREAD_SUSPEND_RESUME: u32 = 0x0002;
    const INVALID_HANDLE_VALUE: isize = -1;
    let snapshot = unsafe { CreateToolhelp32Snapshot(SNAP_THREADS, 0) };
    if snapshot as isize == INVALID_HANDLE_VALUE {
        return Err("Could not enumerate process threads.".into());
    }
    let mut entry = ThreadEntry32 {
        size: std::mem::size_of::<ThreadEntry32>() as u32,
        usage: 0,
        thread_id: 0,
        owner_process_id: 0,
        base_priority: 0,
        delta_priority: 0,
        flags: 0,
    };
    let mut found = 0usize;
    let mut failed = false;
    let mut has_entry = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    while has_entry {
        if entry.owner_process_id == process_id {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.thread_id) };
            if !thread.is_null() {
                found += 1;
                let result = unsafe {
                    if suspend {
                        SuspendThread(thread)
                    } else {
                        ResumeThread(thread)
                    }
                };
                if result == u32::MAX {
                    failed = true;
                }
                unsafe {
                    CloseHandle(thread);
                }
            }
        }
        entry.size = std::mem::size_of::<ThreadEntry32>() as u32;
        has_entry = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    unsafe {
        CloseHandle(snapshot);
    }
    if found == 0 || failed {
        Err(format!(
            "Could not {} all XvdTool threads.",
            if suspend { "suspend" } else { "resume" }
        ))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn suspend_process(_process_id: u32, _suspend: bool) -> Result<(), String> {
    Err("Pausing extraction is supported only on Windows.".into())
}

fn register_extracted_package(
    game_dir: &Path,
    control: &Control,
    tx: &mpsc::Sender<DownloadEvent>,
    id: u64,
) -> Result<bool, String> {
    let manifest = game_dir.join("appxmanifest.xml");
    if !manifest.is_file() {
        publish_registration_result(tx, id, Err("appxmanifest.xml was not found.".into()));
        return Ok(true);
    }
    let _ = tx.send(DownloadEvent::Registering { id });
    let wdapp = resolve_asset_dir("OnlineFix", "UD_ONLINEFIX_DIR", "../original/OnlineFix")
        .join("wdapp.exe");
    if !wdapp.is_file() {
        publish_registration_result(
            tx,
            id,
            Err(format!("wdapp.exe was not found at {}.", wdapp.display())),
        );
        return Ok(true);
    }
    let previous_mode = read_developer_mode();
    if let Err(error) = write_developer_mode(Some(1)) {
        publish_registration_result(
            tx,
            id,
            Err(format!("Developer Mode could not be enabled: {error}")),
        );
        return Ok(true);
    }
    let mut wdapp_command = Command::new(wdapp);
    wdapp_command
        .current_dir(game_dir)
        .arg("register")
        .arg(manifest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    hide_child_console(&mut wdapp_command);
    let mut child = match wdapp_command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let restore = write_developer_mode(previous_mode);
            let message = format!(
                "wdapp could not start: {error}{}",
                restore
                    .err()
                    .map(|e| format!("; Developer Mode restore failed: {e}"))
                    .unwrap_or_default()
            );
            publish_registration_result(tx, id, Err(message));
            return Ok(true);
        }
    };
    let mut suspended = false;
    let registration = loop {
        match control.get() {
            ControlState::Cancelled => {
                if suspended {
                    let _ = suspend_process(child.id(), false);
                }
                let _ = child.kill();
                let _ = child.wait();
                let _ = write_developer_mode(previous_mode);
                return Ok(false);
            }
            ControlState::Paused if !suspended => {
                if let Err(error) = suspend_process(child.id(), true) {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = write_developer_mode(previous_mode);
                    return Err(format!("Could not pause package registration: {error}"));
                }
                suspended = true;
            }
            ControlState::Running if suspended => {
                if let Err(error) = suspend_process(child.id(), false) {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = write_developer_mode(previous_mode);
                    publish_registration_result(
                        tx,
                        id,
                        Err(format!("Package registration could not resume: {error}")),
                    );
                    return Ok(true);
                }
                suspended = false;
                let _ = tx.send(DownloadEvent::Registering { id });
            }
            _ => {}
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                if suspended {
                    let _ = suspend_process(child.id(), false);
                }
                let _ = child.kill();
                let _ = child.wait();
                let _ = write_developer_mode(previous_mode);
                publish_registration_result(
                    tx,
                    id,
                    Err(format!("wdapp could not be monitored: {error}")),
                );
                return Ok(true);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let restore_result = write_developer_mode(previous_mode);
    let mut result = if registration.success() {
        Ok(())
    } else {
        Err(format!(
            "wdapp exited with code {}.",
            registration.code().unwrap_or(-1)
        ))
    };
    if let Err(error) = restore_result {
        result = Err(format!(
            "{}; Developer Mode could not be restored: {error}",
            result
                .err()
                .unwrap_or_else(|| "Game registered successfully".into())
        ));
    }
    publish_registration_result(tx, id, result);
    Ok(true)
}

fn publish_registration_result(
    tx: &mpsc::Sender<DownloadEvent>,
    id: u64,
    result: Result<(), String>,
) {
    let _ = tx.send(DownloadEvent::RegistrationResult { id, result });
}

fn extract_msixvc(
    package: &Path,
    output_dir: &Path,
    product_id: &str,
    package_bytes: u64,
    control: &Control,
    tx: &mpsc::Sender<DownloadEvent>,
    id: u64,
) -> Result<bool, String> {
    let tool_dir = resolve_asset_dir("Xvd", "UD_XVD_TOOL_DIR", "../original/Xvd");
    let executable = tool_dir.join("XvdTool.Streaming.exe");
    if !executable.is_file() {
        return Err(format!(
            "XvdTool was not found at {}. Set UD_XVD_TOOL_DIR to its folder.",
            executable.display()
        ));
    }
    let cik_url = fetch_cik_url(product_id)?;
    let mut command = Command::new(&executable);
    command
        .current_dir(&tool_dir)
        .arg("extract")
        .arg(package)
        .arg("-o")
        .arg(output_dir)
        .args(["-n", "-j"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_child_console(&mut command);
    if let Some(url) = cik_url {
        command
            .arg("--cik-url")
            .arg(url.as_str())
            .env("UD_CIK_AUTH_TOKEN", crate::net::API_KEY);
    }
    let mut child = command.spawn().map_err(|e| {
        format!("Could not start XvdTool: {e}. Check that the .NET 8 runtime is installed.")
    })?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let progress_tx = tx.clone();
    let progress_reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.contains("0x40000003") {
                continue;
            }
            if let Some(percent) = line
                .split_once(':')
                .and_then(|(_, tail)| tail.trim().trim_end_matches('%').parse::<u64>().ok())
            {
                let _ = progress_tx.send(DownloadEvent::ExtractionProgress { id, percent });
            }
        }
    });
    let error_reader = std::thread::spawn(move || {
        BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
            .collect::<Vec<_>>()
            .join("\n")
    });
    let mut suspended = false;
    loop {
        match control.get() {
            ControlState::Cancelled => {
                if suspended {
                    let _ = suspend_process(child.id(), false);
                }
                let _ = child.kill();
                let _ = child.wait();
                let _ = progress_reader.join();
                let _ = error_reader.join();
                return Ok(false);
            }
            ControlState::Paused if !suspended => {
                if let Err(error) = suspend_process(child.id(), true) {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = progress_reader.join();
                    let _ = error_reader.join();
                    return Err(format!("Could not pause XvdTool: {error}"));
                }
                suspended = true;
            }
            ControlState::Running if suspended => {
                if let Err(error) = suspend_process(child.id(), false) {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = progress_reader.join();
                    let _ = error_reader.join();
                    return Err(format!("Could not resume XvdTool: {error}"));
                }
                suspended = false;
                let _ = tx.send(DownloadEvent::Extracting { id, package_bytes });
            }
            _ => {}
        }
        let child_status = match child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                if suspended {
                    let _ = suspend_process(child.id(), false);
                }
                let _ = child.kill();
                let _ = child.wait();
                let _ = progress_reader.join();
                let _ = error_reader.join();
                return Err(format!("Could not monitor XvdTool: {error}"));
            }
        };
        if let Some(status) = child_status {
            let _ = progress_reader.join();
            let stderr = error_reader.join().unwrap_or_default();
            if status.success() {
                let _ = tx.send(DownloadEvent::ExtractionProgress { id, percent: 100 });
                return Ok(true);
            }
            return Err(if stderr.trim().is_empty() {
                format!("XvdTool exited with {status}.")
            } else {
                format!("XvdTool extraction failed: {}", stderr.trim())
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn fetch_cik_url(product_id: &str) -> Result<Option<Url>, String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(crate::net::USER_AGENT)
        .build()
        .map_err(|e| e.to_string())?;
    let mapping = client
        .get(format!("https://xboxapi.netlify.app/mapping/{product_id}"))
        .bearer_auth(crate::net::API_KEY)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json::<serde_json::Value>())
        .map_err(|e| format!("Could not resolve the package decryption key: {e}"))?;
    let Some(guid) = mapping.get("cik").and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    let valid_guid = guid.len() == 36
        && guid.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        });
    if !valid_guid {
        return Err("XboxApi returned an invalid CIK identifier.".into());
    }
    let mut url = Url::parse("https://xboxapi.netlify.app/cik/")
        .map_err(|error| format!("Could not construct the XboxApi CIK URL: {error}"))?;
    url.path_segments_mut()
        .map_err(|_| "Could not construct the XboxApi CIK URL.")?
        .push(guid);
    Ok(Some(url))
}

fn valid_product_id(value: &str) -> bool {
    value.len() >= 8
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn resolve_xbox_package(product_id: &str) -> Result<(String, u64), String> {
    const MARKET: &str = "US";
    const LANGUAGES: &str = "en-US,neutral";
    download_log(format!(
        "Resolving Xbox MSIXVC package for product_id={product_id}, market={MARKET}, languages={LANGUAGES}"
    ));
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(crate::net::USER_AGENT)
        .build()
        .map_err(|e| e.to_string())?;
    let details_url = client
        .get("https://displaycatalog.mp.microsoft.com/v7.0/products")
        .query(&[
            ("bigIds", product_id),
            ("market", MARKET),
            ("languages", LANGUAGES),
            ("MS-CV", "DGU1mcuYo0WMMp+F.1"),
        ]);
    let details_request_url = details_url
        .try_clone()
        .map(|request| request.build())
        .transpose()
        .ok()
        .flatten()
        .map(|request| safe_url(request.url()))
        .unwrap_or_else(|| "<Microsoft catalog request>".into());
    download_log(format!(
        "GET Microsoft package metadata {details_request_url}"
    ));
    let details_response = details_url.send().map_err(|error| {
        let error = error.without_url().to_string();
        download_log(format!(
            "Microsoft package metadata transport error: {error}"
        ));
        format!("Could not get Microsoft package information: {error}")
    })?;
    let details_status = details_response.status();
    if !details_status.is_success() {
        let body = details_response.text().unwrap_or_default();
        let excerpt = body_excerpt(&body);
        download_log(format!(
            "Microsoft package metadata HTTP {details_status}: {excerpt}"
        ));
        return Err(format!(
            "Microsoft package metadata request returned {details_status}."
        ));
    }
    let details = details_response
        .json::<serde_json::Value>()
        .map_err(|error| {
            download_log(format!(
                "Could not parse Microsoft package metadata: {error}"
            ));
            format!("Could not parse Microsoft package information: {error}")
        })?;
    let products = details
        .get("Products")
        .and_then(|v| v.as_array())
        .ok_or("Microsoft returned no product details.")?;
    let mut package = None;
    for sku in products
        .first()
        .and_then(|p| p.get("DisplaySkuAvailabilities"))
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        for candidate in sku
            .pointer("/Sku/Properties/Packages")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            if candidate.get("PackageFormat").and_then(|v| v.as_str()) == Some("MSIXVC") {
                if let Some(content_id) = candidate.get("ContentId").and_then(|v| v.as_str()) {
                    let size = candidate
                        .get("MaxDownloadSizeInBytes")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    download_log(format!(
                        "Microsoft MSIXVC candidate: content_id={content_id}, size={size}"
                    ));
                    package = Some((content_id.to_owned(), size));
                    break;
                }
            }
        }
        if package.is_some() {
            break;
        }
    }
    let (content_id, size) =
        package.ok_or("No downloadable MSIXVC package was found for this product.")?;
    download_log(format!(
        "Microsoft package found: content_id={content_id}, size={size}"
    ));
    let resolver_request = client
        .get("https://xbox.skydevil.xyz/Game/GetGamePackage")
        .query(&[
            ("contentId", content_id.as_str()),
            // The package selected from DisplayCatalog is Windows.Desktop
            // MSIXVC. Xbox Download Assistant uses platform 9 for PC; 0
            // resolves the console package (often only a Blob root here).
            ("platform", "9"),
            ("size", &size.to_string()),
        ])
        .header("accept", "*/*")
        .header("referer", "https://xbox.skydevil.xyz/")
        .header("x-author", "Devil")
        .header("x-organization", "XboxDownload")
        .header(USER_AGENT, XBOX_RESOLVER_USER_AGENT);
    let resolver_url = resolver_request
        .try_clone()
        .map(|request| request.build())
        .transpose()
        .ok()
        .flatten()
        .map(|request| safe_url(request.url()))
        .unwrap_or_else(|| "<Xbox package resolver request>".into());
    download_log(format!("GET Xbox package resolver {resolver_url}"));
    download_log(format!(
        "Xbox resolver headers: accept=*/*, referer=https://xbox.skydevil.xyz/, x-author=Devil, x-organization=XboxDownload, platform=9 (Windows PC), user-agent={}",
        XBOX_RESOLVER_USER_AGENT
    ));
    let resolver_response = resolver_request.send().map_err(|error| {
        let error = error.without_url().to_string();
        download_log(format!("Xbox package resolver transport error: {error}"));
        format!("Could not resolve Xbox package download: {error}")
    })?;
    let resolver_status = resolver_response.status();
    let resolver_body = resolver_response.text().map_err(|error| {
        let error = error.without_url().to_string();
        download_log(format!(
            "Could not read Xbox package resolver response: {error}"
        ));
        format!("Could not read Xbox package resolver response: {error}")
    })?;
    if !resolver_status.is_success() {
        download_log(format!(
            "Xbox package resolver HTTP {resolver_status}: {}",
            body_excerpt(&resolver_body)
        ));
        return Err(format!("Xbox package resolver returned {resolver_status}."));
    }
    let response: serde_json::Value = serde_json::from_str(&resolver_body).map_err(|error| {
        download_log(format!(
            "Invalid Xbox package resolver JSON: {error}; body={}",
            body_excerpt(&resolver_body)
        ));
        format!("Could not parse Xbox package resolver response: {error}")
    })?;
    download_log(format!(
        "Xbox package resolver result: code={}, message={}",
        response
            .get("code")
            .map(|value| value.to_string())
            .unwrap_or_else(|| "<missing>".into()),
        response
            .get("msg")
            .or_else(|| response.get("message"))
            .and_then(|value| value.as_str())
            .unwrap_or("<none>")
    ));
    if response.get("code").and_then(|v| v.as_i64()).or_else(|| {
        response
            .get("code")
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse().ok())
    }) != Some(200)
    {
        return Err(response
            .get("msg")
            .or_else(|| response.get("message"))
            .and_then(|v| v.as_str())
            .unwrap_or("Xbox package service returned no download link.")
            .to_owned());
    }
    let raw_url = response
        .pointer("/data/url")
        .and_then(|v| v.as_str())
        .ok_or("Xbox package service returned no download link.")?;
    let url = Url::parse(raw_url)
        .map_err(|e| format!("Xbox package service returned an invalid URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("Xbox package service returned an unsupported URL.".into());
    }
    let has_path = url
        .path_segments()
        .is_some_and(|segments| segments.into_iter().any(|segment| !segment.is_empty()));
    if url
        .host_str()
        .is_some_and(|host| host.ends_with(".blob.core.windows.net"))
        && !has_path
    {
        download_log(format!(
            "Xbox package resolver returned an unusable Azure Blob base URI: host={}, path={}, query=[{}], url_length={}; no package object path was supplied",
            url.host_str().unwrap_or("<missing>"),
            url.path(),
            query_summary(&url),
            raw_url.len()
        ));
        return Err("Xbox package resolver returned only the Azure Blob storage root, without a package file path. Its download link is incomplete; the package service must be fixed before this game can be downloaded.".into());
    }
    let size = response
        .pointer("/data/size")
        .and_then(|v| v.as_u64())
        .filter(|v| *v > 0)
        .unwrap_or(size);
    download_log(format!(
        "Resolved package URL={} query=[{}], expected_size={size}",
        safe_url(&url),
        query_summary(&url)
    ));
    // Keep the resolver's exact signed URL representation, as the original Electron client does.
    Ok((raw_url.to_owned(), size))
}

fn transfer_xbox_ranges(
    url: &str,
    expected_size: u64,
    destination: &Path,
    partial_path: &Path,
    id: u64,
    control: &Control,
    tx: &mpsc::Sender<DownloadEvent>,
) -> Result<bool, String> {
    const RANGE_BYTES: u64 = 16 * 1024 * 1024;
    const WORKERS: usize = 8;
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(180))
        .user_agent(crate::net::USER_AGENT)
        .build()
        .map_err(|error| format!("Could not create HTTP client: {error}"))?;

    // Confirm that this particular package endpoint honors a bounded byte range. Some Xbox
    // asset hosts return 206 for an open-ended range while ignoring its requested end.
    let probe = client
        .get(url)
        .header(USER_AGENT, XBOX_ASSET_USER_AGENT)
        .header(RANGE, "bytes=0-0")
        .send();
    let (total, supported) = match probe {
        Ok(response) => {
            let range = response
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .and_then(parse_content_range);
            let content_length = response
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok());
            let valid = response.status() == reqwest::StatusCode::PARTIAL_CONTENT
                && range.is_some_and(|(start, end, range_total)| {
                    start == 0
                        && end == 0
                        && range_total > 1
                        && (expected_size == 0 || range_total == expected_size)
                })
                && content_length == Some(1);
            let total = range.map(|(_, _, total)| total).unwrap_or(expected_size);
            drop(response);
            (total, valid)
        }
        Err(error) => {
            download_log(format!(
                "Xbox byte-range probe failed; using the regular stream: {}",
                error.without_url()
            ));
            (expected_size, false)
        }
    };
    if !supported || total == 0 {
        download_log(format!(
            "Xbox server does not confirm bounded ranges; using the regular stream (expected_size={expected_size})"
        ));
        return transfer(url, true, destination, partial_path, id, control, tx);
    }
    download_log(format!(
        "Xbox server confirmed byte ranges: total={total}, segment_size={RANGE_BYTES}, workers={WORKERS}"
    ));

    let mut jobs = VecDeque::new();
    let mut already_received = 0u64;
    let mut segment_start = 0u64;
    while segment_start < total {
        let segment_end = segment_start.saturating_add(RANGE_BYTES - 1).min(total - 1);
        let segment_path = xbox_segment_path(partial_path, segment_start);
        let expected_segment = segment_end - segment_start + 1;
        let mut existing = fs::metadata(&segment_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if existing > expected_segment {
            fs::remove_file(&segment_path)
                .map_err(|error| format!("Could not reset an Xbox download segment: {error}"))?;
            existing = 0;
        }
        if existing == expected_segment {
            already_received = already_received.saturating_add(existing);
        } else {
            jobs.push_back((segment_start, segment_end));
            already_received = already_received.saturating_add(existing);
        }
        segment_start = segment_end + 1;
    }
    let progress_received = Arc::new(AtomicU64::new(already_received));
    let speed_meter = Arc::new(Mutex::new(SpeedMeter::new()));
    let job_count = jobs.len();
    if job_count > 0 {
        let jobs = Arc::new(Mutex::new(jobs));
        let (result_tx, result_rx) = mpsc::channel();
        let worker_count = WORKERS.min(job_count);
        std::thread::scope(|scope| -> Result<(), String> {
            for _ in 0..worker_count {
                let jobs = Arc::clone(&jobs);
                let client = client.clone();
                let result_tx = result_tx.clone();
                let progress_received = Arc::clone(&progress_received);
                let speed_meter = Arc::clone(&speed_meter);
                scope.spawn(move || {
                    loop {
                        if control.get() != ControlState::Running {
                            break;
                        }
                        let job = jobs
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .pop_front();
                        let Some((start, end)) = job else { break };
                        let path = xbox_segment_path(partial_path, start);
                        let result = download_xbox_range(
                            &client,
                            url,
                            total,
                            start,
                            end,
                            &path,
                            id,
                            control,
                            tx,
                            &progress_received,
                            &speed_meter,
                        );
                        let _ = result_tx.send(result);
                    }
                });
            }
            drop(result_tx);
            let mut completed = 0;
            let mut first_error = None;
            while completed < job_count {
                match result_rx.recv() {
                    Ok(Ok(())) => completed += 1,
                    Ok(Err(error)) => {
                        completed += 1;
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                    Err(_) => break,
                }
            }
            if control.get() == ControlState::Paused {
                return Err("paused".into());
            }
            if control.get() == ControlState::Cancelled {
                return Err("cancelled".into());
            }
            if let Some(error) = first_error {
                return Err(error);
            }
            if completed != job_count {
                return Err("Xbox range workers stopped unexpectedly.".into());
            }
            Ok(())
        })?;
    }

    if control.get() == ControlState::Cancelled {
        remove_xbox_segments(partial_path, total)?;
        return Ok(false);
    }
    if !control.wait_until_resumed() {
        remove_xbox_segments(partial_path, total)?;
        return Ok(false);
    }
    let mut output = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(partial_path)
        .map_err(|error| format!("Could not assemble the Xbox package: {error}"))?;
    let mut segment_start = 0u64;
    let mut buffer = vec![0; 1024 * 1024];
    while segment_start < total {
        if !control.wait_until_resumed() {
            drop(output);
            remove_partial(partial_path)?;
            remove_xbox_segments(partial_path, total)?;
            return Ok(false);
        }
        let segment_end = segment_start.saturating_add(RANGE_BYTES - 1).min(total - 1);
        let segment_path = xbox_segment_path(partial_path, segment_start);
        let expected_segment = segment_end - segment_start + 1;
        let metadata = fs::metadata(&segment_path)
            .map_err(|error| format!("Missing Xbox download segment: {error}"))?;
        if metadata.len() != expected_segment {
            return Err("An Xbox download segment is incomplete.".into());
        }
        let mut input = File::open(&segment_path)
            .map_err(|error| format!("Could not open Xbox download segment: {error}"))?;
        loop {
            let count = input
                .read(&mut buffer)
                .map_err(|error| format!("Could not read Xbox download segment: {error}"))?;
            if count == 0 {
                break;
            }
            output
                .write_all(&buffer[..count])
                .map_err(|error| format!("Could not assemble Xbox package: {error}"))?;
        }
        segment_start = segment_end + 1;
    }
    output
        .flush()
        .and_then(|_| output.sync_all())
        .map_err(|error| format!("Could not finish assembling Xbox package: {error}"))?;
    drop(output);
    remove_xbox_segments(partial_path, total)?;
    publish_progress(tx, id, total, total, 0);
    finish_file(partial_path, destination)?;
    Ok(true)
}

fn download_xbox_range(
    client: &Client,
    url: &str,
    total: u64,
    start: u64,
    end: u64,
    segment_path: &Path,
    id: u64,
    control: &Control,
    tx: &mpsc::Sender<DownloadEvent>,
    progress_received: &AtomicU64,
    speed_meter: &Mutex<SpeedMeter>,
) -> Result<(), String> {
    let expected = end - start + 1;
    let mut offset = fs::metadata(segment_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if offset > expected {
        fs::remove_file(segment_path)
            .map_err(|error| format!("Could not reset Xbox range segment: {error}"))?;
        offset = 0;
    }
    let mut retries = 0u8;
    while offset < expected {
        if control.get() == ControlState::Paused {
            return Err("paused".into());
        }
        if control.get() == ControlState::Cancelled {
            return Err("cancelled".into());
        }
        let request_start = start + offset;
        let mut response = client
            .get(url)
            .header(USER_AGENT, XBOX_ASSET_USER_AGENT)
            .header(RANGE, format!("bytes={request_start}-{end}"))
            .send()
            .map_err(|error| format!("Download request failed: {}", error.without_url()))?;
        if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(format!(
                "Download request failed: Xbox range request returned {}.",
                response.status()
            ));
        }
        let range = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_content_range)
            .ok_or_else(|| "Xbox server returned an invalid Content-Range.".to_owned())?;
        if range != (request_start, end, total) {
            return Err(format!(
                "Xbox server returned unexpected Content-Range bytes {}-{}/{} for requested bytes {request_start}-{end}/{total}.",
                range.0, range.1, range.2
            ));
        }
        let mut output = OpenOptions::new()
            .create(true)
            .append(true)
            .open(segment_path)
            .map_err(|error| format!("Could not write Xbox range segment: {error}"))?;
        let mut buffer = vec![0; 256 * 1024];
        let mut last_report = Instant::now();
        let mut read_error = None;
        loop {
            if control.get() != ControlState::Running {
                output.flush().map_err(|error| error.to_string())?;
                return Err(if control.get() == ControlState::Paused {
                    "paused".into()
                } else {
                    "cancelled".into()
                });
            }
            match response.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if offset.saturating_add(count as u64) > expected {
                        return Err("Xbox server sent more bytes than requested.".into());
                    }
                    output
                        .write_all(&buffer[..count])
                        .map_err(|error| format!("Could not write Xbox range segment: {error}"))?;
                    offset += count as u64;
                    progress_received.fetch_add(count as u64, Ordering::Relaxed);
                    speed_meter
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .add(count as u64);
                    if last_report.elapsed() >= Duration::from_millis(250) {
                        let speed = speed_meter
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .speed();
                        publish_progress(
                            tx,
                            id,
                            progress_received.load(Ordering::Relaxed).min(total),
                            total,
                            speed,
                        );
                        last_report = Instant::now();
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    read_error = Some(error.to_string());
                    break;
                }
            }
        }
        output
            .flush()
            .map_err(|error| format!("Could not flush Xbox range segment: {error}"))?;
        drop(output);
        if offset == expected {
            return Ok(());
        }
        retries = retries.saturating_add(1);
        if retries > MAX_DOWNLOAD_READ_RETRIES {
            return Err(format!(
                "Could not read the download after {MAX_DOWNLOAD_READ_RETRIES} retries: {} bytes of range received{}",
                offset,
                read_error
                    .map(|error| format!(" ({error})"))
                    .unwrap_or_default()
            ));
        }
        let pause = Duration::from_millis(250 * u64::from(retries.min(20)));
        let until = Instant::now() + pause;
        while Instant::now() < until {
            if control.get() != ControlState::Running {
                return Err(if control.get() == ControlState::Paused {
                    "paused".into()
                } else {
                    "cancelled".into()
                });
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    Ok(())
}

fn xbox_segment_path(partial_path: &Path, start: u64) -> PathBuf {
    let mut name = partial_path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".range.{start}"));
    partial_path.with_file_name(name)
}

fn has_xbox_range_data(partial_path: &Path) -> bool {
    let Some(parent) = partial_path.parent() else {
        return false;
    };
    let Some(prefix) = partial_path.file_name() else {
        return false;
    };
    let prefix = format!("{}.range.", prefix.to_string_lossy());
    fs::read_dir(parent)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|entry| {
            entry.file_name().to_string_lossy().starts_with(&prefix)
                && entry
                    .metadata()
                    .map(|metadata| metadata.len() > 0)
                    .unwrap_or(false)
        })
}

fn remove_xbox_segments(partial_path: &Path, total: u64) -> Result<(), String> {
    const RANGE_BYTES: u64 = 16 * 1024 * 1024;
    let mut start = 0u64;
    while start < total {
        let path = xbox_segment_path(partial_path, start);
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Could not remove Xbox range segment: {error}")),
        }
        start = start.saturating_add(RANGE_BYTES);
    }
    Ok(())
}

fn transfer(
    url: &str,
    xbox_package: bool,
    destination: &Path,
    partial_path: &Path,
    id: u64,
    control: &Control,
    tx: &mpsc::Sender<DownloadEvent>,
) -> Result<bool, String> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .user_agent(crate::net::USER_AGENT)
        .build()
        .map_err(|error| {
            download_log(format!("Could not create HTTP client: {error}"));
            format!("Could not create HTTP client: {error}")
        })?;
    let mut received = fs::metadata(partial_path)
        .map_err(|error| format!("Could not inspect partial download: {error}"))?
        .len();
    let mut total = 0;
    let mut read_retries = 0u8;

    'requests: loop {
        if !control.wait_until_resumed() {
            remove_partial(partial_path)
                .map_err(|error| format!("Could not remove partial download: {error}"))?;
            return Ok(false);
        }
        if total > 0 && received == total {
            finish_file(partial_path, destination)?;
            return Ok(true);
        }

        let mut request = client.get(url);
        if xbox_package {
            request = request.header(USER_AGENT, XBOX_ASSET_USER_AGENT);
        }
        if received > 0 || xbox_package {
            request = request.header(RANGE, format!("bytes={received}-"));
        }
        let parsed_url = Url::parse(url).ok();
        download_log(format!(
            "GET {} query=[{}] range_start={} range_requested={} user_agent={} destination={}",
            parsed_url
                .as_ref()
                .map(safe_url)
                .unwrap_or_else(|| "<invalid URL>".into()),
            parsed_url
                .as_ref()
                .map(query_summary)
                .unwrap_or_else(|| "<unavailable>".into()),
            received,
            received > 0 || xbox_package,
            if xbox_package {
                XBOX_ASSET_USER_AGENT
            } else {
                crate::net::USER_AGENT
            },
            destination.display()
        ));
        let mut response = request.send().map_err(|error| {
            let error = error.without_url().to_string();
            download_log(format!("Download request transport error: {error}"));
            format!("Download request failed: {error}")
        })?;
        let status = response.status();
        download_log(format!(
            "Download response status={} final_url={} query=[{}] content_type={} content_length={} content_range={} accept_ranges={} server={}",
            status,
            safe_url(response.url()),
            query_summary(response.url()),
            response_header(&response, "content-type"),
            response_header(&response, "content-length"),
            response_header(&response, "content-range"),
            response_header(&response, "accept-ranges"),
            response_header(&response, "server")
        ));
        if !status.is_success() {
            let body = response.text().unwrap_or_else(|error| {
                format!("<could not read error response: {}>", error.without_url())
            });
            let excerpt = body_excerpt(&body);
            download_log(format!(
                "Download HTTP error status={status} body={excerpt}"
            ));
            return Err(format!(
                "Download server responded with {status}. Details are in %APPDATA%\\UniversalDownloader\\app.log."
            ));
        }

        if received > 0 && status != reqwest::StatusCode::PARTIAL_CONTENT {
            File::create(partial_path)
                .and_then(|file| file.set_len(0))
                .map_err(|error| format!("Could not restart the download: {error}"))?;
            received = 0;
        } else if status == reqwest::StatusCode::PARTIAL_CONTENT {
            let range = response
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| "The server returned an invalid byte range.".to_owned())?;
            download_log(format!(
                "Partial response Content-Range={range}; expected start={received}"
            ));
            if parse_content_range_start(range) != Some(received) {
                return Err("The server returned an invalid byte range.".into());
            }
        }

        total = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .and_then(parse_content_range_total)
            .or_else(|| {
                response
                    .headers()
                    .get(CONTENT_LENGTH)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .map(|length| length.saturating_add(received))
            })
            .unwrap_or(total);

        let mut output = OpenOptions::new()
            .create(true)
            .append(true)
            .open(partial_path)
            .map_err(|error| format!("Could not write the download: {error}"))?;
        let mut buffer = vec![0; 64 * 1024];
        let mut last_report = Instant::now();
        let mut last_bytes = received;

        loop {
            if control.get() == ControlState::Cancelled {
                output
                    .flush()
                    .map_err(|error| format!("Could not flush the partial download: {error}"))?;
                drop(output);
                remove_partial(partial_path)
                    .map_err(|error| format!("Could not remove partial download: {error}"))?;
                return Ok(false);
            }
            if control.get() == ControlState::Paused {
                output
                    .flush()
                    .map_err(|error| format!("Could not pause the download: {error}"))?;
                drop(output);
                break;
            }

            match response.read(&mut buffer) {
                Ok(0) => {
                    output.flush().map_err(|error| {
                        format!("Could not finish writing the download: {error}")
                    })?;
                    drop(output);
                    if total > 0 && received < total {
                        return Err(format!(
                            "The connection ended early ({received} of {total} bytes received)."
                        ));
                    }
                    if control.get() == ControlState::Cancelled {
                        remove_partial(partial_path).map_err(|error| {
                            format!("Could not remove partial download: {error}")
                        })?;
                        return Ok(false);
                    }
                    publish_progress(tx, id, received, total, 0);
                    finish_file(partial_path, destination)?;
                    return Ok(true);
                }
                Ok(count) => {
                    output
                        .write_all(&buffer[..count])
                        .map_err(|error| format!("Could not write the download: {error}"))?;
                    received = received.saturating_add(count as u64);
                    if total > 0 && received > total {
                        return Err("The server sent more data than expected.".into());
                    }
                    if last_report.elapsed() >= Duration::from_millis(250) {
                        let elapsed = last_report.elapsed().as_secs_f64();
                        let speed = if elapsed > 0.0 {
                            (received.saturating_sub(last_bytes) as f64 / elapsed) as u64
                        } else {
                            0
                        };
                        publish_progress(tx, id, received, total, speed);
                        last_report = Instant::now();
                        last_bytes = received;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    let error = error.to_string();
                    output.flush().map_err(|flush_error| {
                        format!(
                            "Could not flush partial download after a read error: {flush_error}"
                        )
                    })?;
                    drop(output);

                    if total > 0 && received >= total {
                        publish_progress(tx, id, received, total, 0);
                        finish_file(partial_path, destination)?;
                        return Ok(true);
                    }
                    if read_retries >= MAX_DOWNLOAD_READ_RETRIES {
                        return Err(format!(
                            "Could not read the download after {read_retries} retries: {error}"
                        ));
                    }

                    read_retries += 1;
                    let delay_seconds = u64::from(read_retries.min(5));
                    download_log(format!(
                        "Download body read failed at {received}/{total} bytes; retry {read_retries}/{MAX_DOWNLOAD_READ_RETRIES} in {delay_seconds}s: {error}"
                    ));
                    for _ in 0..delay_seconds * 4 {
                        if !control.wait_until_resumed() {
                            return Ok(false);
                        }
                        std::thread::sleep(Duration::from_millis(250));
                    }
                    continue 'requests;
                }
            }
        }
    }
}

fn publish_progress(
    tx: &mpsc::Sender<DownloadEvent>,
    id: u64,
    received: u64,
    total: u64,
    speed: u64,
) {
    let _ = tx.send(DownloadEvent::Progress {
        id,
        received,
        total,
        speed,
    });
}

fn parse_content_range_start(value: &str) -> Option<u64> {
    value
        .strip_prefix("bytes ")?
        .split_once('-')?
        .0
        .parse()
        .ok()
}

fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let range = value.strip_prefix("bytes ")?;
    let (positions, total) = range.split_once('/')?;
    let (start, end) = positions.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

fn parse_content_range_total(value: &str) -> Option<u64> {
    value.rsplit_once('/')?.1.parse().ok()
}

fn partial_path(destination: &Path) -> PathBuf {
    let mut name = destination.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    destination.with_file_name(name)
}

fn remove_partial(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn finish_file(partial: &Path, destination: &Path) -> Result<(), String> {
    match fs::hard_link(partial, destination) {
        Ok(()) => fs::remove_file(partial).map_err(|error| {
            format!("Downloaded file but could not remove temporary file: {error}")
        }),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Err(format!(
            "The destination already exists: {}",
            destination.display()
        )),
        Err(_) => {
            let mut input = File::open(partial)
                .map_err(|error| format!("Could not open completed download: {error}"))?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)
                .map_err(|error| {
                    if error.kind() == io::ErrorKind::AlreadyExists {
                        format!("The destination already exists: {}", destination.display())
                    } else {
                        format!("Could not create destination file: {error}")
                    }
                })?;
            if let Err(error) = io::copy(&mut input, &mut output).and_then(|_| output.sync_all()) {
                let _ = fs::remove_file(destination);
                return Err(format!("Could not finalize the download: {error}"));
            }
            fs::remove_file(partial).map_err(|error| {
                format!("Downloaded file but could not remove temporary file: {error}")
            })
        }
    }
}

fn sanitize_filename(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() || r#"<>:"/\|?*"#.contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect::<String>()
        .trim()
        .trim_matches('.')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn sanitizes_windows_file_names() {
        assert_eq!(sanitize_filename(r#" My/Game:?.zip "#), "My_Game__.zip");
        assert!(sanitize_filename("...").is_empty());
    }

    #[test]
    fn reads_content_range_fields() {
        assert_eq!(parse_content_range_start("bytes 12-24/100"), Some(12));
        assert_eq!(parse_content_range_total("bytes 12-24/100"), Some(100));
        assert_eq!(parse_content_range_total("bytes 12-24/*"), None);
    }

    #[test]
    fn transfers_direct_http_payload_without_overwriting() {
        let body = b"authorized test payload";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                }
                drop(reader);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
            }
        });

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let folder = std::env::temp_dir().join(format!("universal-downloader-{nonce}"));
        fs::create_dir_all(&folder).unwrap();
        let destination = folder.join("sample.zip");
        let partial = partial_path(&destination);
        File::create(&partial).unwrap();
        let (tx, _rx) = mpsc::channel();
        let control = Control::new();
        let url = format!("http://{address}/sample.zip");
        assert!(transfer(&url, false, &destination, &partial, 1, &control, &tx).unwrap());
        let second_partial = partial_path(&destination);
        File::create(&second_partial).unwrap();
        assert!(transfer(&url, false, &destination, &second_partial, 2, &control, &tx).is_err());
        server.join().unwrap();
        assert_eq!(fs::read(&destination).unwrap(), body);
        fs::remove_dir_all(folder).unwrap();
    }
}
