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
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const XBOX_RESOLVER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36";
const XBOX_ASSET_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";

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
            if state.active < 2 && state.waiting.front() == Some(&id) {
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

    pub fn start(&mut self, url: &str, name: &str, folder: &str) -> Result<(), String> {
        let url = Url::parse(url.trim()).map_err(|error| format!("Invalid URL: {error}"))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err("Only direct HTTP(S) URLs are supported.".into());
        }

        let folder = PathBuf::from(folder.trim());
        if !folder.is_absolute() {
            return Err("Choose an absolute download folder.".into());
        }
        fs::create_dir_all(&folder)
            .map_err(|error| format!("Could not create the download folder: {error}"))?;

        let url_name = url
            .path_segments()
            .and_then(Iterator::last)
            .unwrap_or_default();
        let name = if name.trim().is_empty() {
            url_name
        } else {
            name.trim()
        };
        let display_name = name.to_owned();
        let mut name = sanitize_filename(name);
        if name.is_empty() {
            return Err("Enter a file name or use a URL that includes one.".into());
        }
        if Path::new(&name).extension().is_none()
            && let Some(extension) = Path::new(url_name).extension()
        {
            name.push('.');
            name.push_str(&extension.to_string_lossy());
        }
        let path = folder.join(&name);
        if path.exists() {
            return Err(format!("The file already exists: {}", path.display()));
        }
        let id = self.next_id;
        let next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| "No more download IDs are available.".to_owned())?;
        let partial_path = partial_path(&path);
        let partial = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial_path)
            .map_err(|error| {
                if error.kind() == io::ErrorKind::AlreadyExists {
                    format!(
                        "A partial download already exists: {}",
                        partial_path.display()
                    )
                } else {
                    format!("Could not create the download file: {error}")
                }
            })?;
        drop(partial);

        self.next_id = next_id;
        let control = Arc::new(Control::new());
        let entry = Download {
            id,
            name: display_name,
            path: path.clone(),
            received: 0,
            total: 0,
            speed: 0,
            status: DownloadStatus::Queued,
            downloaded_size: 0,
            registration: None,
            control: Arc::clone(&control),
            product_id: None,
            epic_id: None,
            restored: false,
        };
        self.entries.push(entry);
        self.persist_downloads();
        self.notice.clear();

        let tx = self.tx.clone();
        let queue = Arc::clone(&self.queue);
        queue.enqueue(id);
        let url = url.to_string();
        std::thread::spawn(move || {
            let result = if let Some(_permit) = queue.acquire(id, &control) {
                transfer(&url, false, &path, &partial_path, id, &control, &tx)
            } else {
                Err("cancelled".to_owned())
            };
            let result = match result {
                Ok(true) => Ok(()),
                Ok(false) => Err("cancelled".to_owned()),
                Err(_) if control.get() == ControlState::Cancelled => {
                    match remove_partial(&partial_path) {
                        Ok(()) => {
                            let _ = fs::remove_file(&path);
                            Err("cancelled".to_owned())
                        }
                        Err(error) => Err(format!("Could not remove partial download: {error}")),
                    }
                }
                Err(error) => {
                    let cleanup = remove_partial(&partial_path);
                    Err(match cleanup {
                        Ok(()) => error,
                        Err(cleanup_error) => {
                            format!("{error}; could not remove partial file: {cleanup_error}")
                        }
                    })
                }
            };
            let _ = tx.send(DownloadEvent::Finished { id, result });
        });
        Ok(())
    }

    /// Résout un paquet Xbox pour le produit puis télécharge son MSIXVC dans son dossier.
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
    let mut received_chunks = 0u64;
    let cache_dir = std::env::temp_dir()
        .join("UniversalDownloader")
        .join(format!("epic-chunks-{id}"));
    fs::create_dir_all(&cache_dir)
        .map_err(|e| format!("Could not create Epic chunk cache: {e}"))?;
    let _cache_guard = EpicChunkCache(cache_dir.clone());
    let mut remaining = std::collections::HashMap::<[u32; 4], usize>::new();
    for file in &files {
        for part in &file.parts {
            *remaining.entry(part.guid).or_default() += 1;
        }
    }
    let mut cached = std::collections::HashMap::<[u32; 4], PathBuf>::new();
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
            let chunk = by_guid.get(&part.guid).ok_or_else(|| {
                format!(
                    "Epic manifest references an unknown chunk for {}",
                    file.name
                )
            })?;
            let path = epic_chunk_path(chunk, feature);
            if !cached.contains_key(&part.guid) {
                let mut fetched = None;
                for base in &game.cdn_bases {
                    let url = format!("{}/{}", base.trim_end_matches('/'), path);
                    match client
                        .get(&url)
                        .send()
                        .and_then(|r| r.error_for_status())
                        .and_then(|r| r.bytes())
                    {
                        Ok(data) => {
                            fetched = Some(data.to_vec());
                            break;
                        }
                        Err(_) => {}
                    }
                    if fetched.is_some() {
                        break;
                    }
                }
                let raw = fetched.ok_or_else(|| {
                    format!("Could not fetch Epic chunk {} from available CDNs.", path)
                })?;
                let cache_path = cache_dir.join(format!(
                    "{:08X}{:08X}{:08X}{:08X}.chunk",
                    part.guid[0], part.guid[1], part.guid[2], part.guid[3]
                ));
                fs::write(&cache_path, raw)
                    .map_err(|e| format!("Could not cache Epic chunk: {e}"))?;
                cached.insert(part.guid, cache_path);
                received_chunks = received_chunks.saturating_add(chunk.size);
                publish_progress(&tx, id, received_chunks, bytes_total, 0);
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
            let left = remaining.get_mut(&part.guid).unwrap();
            *left -= 1;
            if *left == 0 {
                if let Some(path) = cached.remove(&part.guid) {
                    let _ = fs::remove_file(path);
                }
            }
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
            if !transfer(&url, true, &destination, &partial_path, id, &control, &tx)? {
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
    let output = Command::new("reg.exe")
        .args([
            "query",
            r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock",
            "/v",
            "AllowDevelopmentWithoutDevLicense",
        ])
        .output()
        .ok()?;
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
    let direct = Command::new("reg.exe")
        .args(&args)
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
    let elevated = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
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
    let mut child = match Command::new(wdapp)
        .current_dir(game_dir)
        .arg("register")
        .arg(manifest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
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
    let cik = fetch_cik(product_id)?;
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
    if let Some(path) = cik.as_deref() {
        command.arg("-c").arg(path);
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

fn fetch_cik(product_id: &str) -> Result<Option<PathBuf>, String> {
    const API_KEY: &str = crate::net::API_KEY;
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(crate::net::USER_AGENT)
        .build()
        .map_err(|e| e.to_string())?;
    let mapping = client
        .get(format!("https://xboxapi.netlify.app/mapping/{product_id}"))
        .bearer_auth(API_KEY)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json::<serde_json::Value>())
        .map_err(|e| format!("Could not resolve the package decryption key: {e}"))?;
    let Some(guid) = mapping.get("cik").and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("UniversalDownloader")
        .join("Cik");
    fs::create_dir_all(&base).map_err(|e| format!("Could not create the key cache: {e}"))?;
    let path = base.join(format!("{guid}.cik"));
    if fs::metadata(&path).is_ok_and(|m| m.len() == 48) {
        return Ok(Some(path));
    }
    let response = client
        .get(format!("https://xboxapi.netlify.app/cik/{guid}"))
        .bearer_auth(API_KEY)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("Could not download the package decryption key: {e}"))?;
    let bytes = response.bytes().map_err(|e| e.to_string())?;
    let decoded = if bytes.first() == Some(&b'{') {
        let wrapper: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|e| format!("Invalid key response: {e}"))?;
        let body = wrapper
            .get("body")
            .and_then(|v| v.as_str())
            .ok_or("Key response did not include key data.")?;
        decode_base64(body)?
    } else {
        bytes.to_vec()
    };
    if decoded.len() != 48 {
        return Err(format!(
            "Package key has an unexpected size ({} bytes).",
            decoded.len()
        ));
    }
    fs::write(&path, decoded).map_err(|e| format!("Could not cache the package key: {e}"))?;
    Ok(Some(path))
}

fn decode_base64(value: &str) -> Result<Vec<u8>, String> {
    let mut output = Vec::with_capacity(value.len() * 3 / 4);
    let mut accumulator = 0u32;
    let mut bits = 0u8;
    for byte in value.bytes().filter(|byte| !byte.is_ascii_whitespace()) {
        if byte == b'=' {
            break;
        }
        let digit = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err("Invalid encoded key: unexpected base64 character.".into()),
        };
        accumulator = (accumulator << 6) | u32::from(digit);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((accumulator >> bits) as u8);
            accumulator &= (1u32 << bits) - 1;
        }
    }
    Ok(output)
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

    loop {
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
                Err(error) => return Err(format!("Could not read the download: {error}")),
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
    fn downloads_direct_http_file_without_overwriting() {
        let body = b"authorized test payload";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
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
        });

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let folder = std::env::temp_dir().join(format!("universal-downloader-{nonce}"));
        let mut manager = DownloadManager::new();
        manager
            .start(
                &format!("http://{address}/sample.zip"),
                "sample",
                &folder.display().to_string(),
            )
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            manager.poll();
            match &manager.entries[0].status {
                DownloadStatus::Completed => break,
                DownloadStatus::Failed(error) => panic!("download failed: {error}"),
                _ if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                _ => panic!("download did not complete in time"),
            }
        }
        server.join().unwrap();
        assert_eq!(fs::read(folder.join("sample.zip")).unwrap(), body);
        assert!(
            manager
                .start(
                    &format!("http://{address}/sample.zip"),
                    "sample",
                    &folder.display().to_string(),
                )
                .is_err()
        );
        assert_eq!(fs::read(folder.join("sample.zip")).unwrap(), body);
        fs::remove_dir_all(folder).unwrap();
    }
}
