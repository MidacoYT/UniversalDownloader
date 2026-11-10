//! Téléchargements HTTP(S) directs, indépendants du catalogue Xbox.
use crate::model::Game;
use reqwest::Url;
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, RANGE, USER_AGENT};
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const XBOX_RESOLVER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36";
const XBOX_ASSET_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";
const MAX_DOWNLOAD_READ_RETRIES: u8 = 8;
/// Nombre de tours complets sur la liste des CDN avant d'abandonner un chunk Epic.
const EPIC_CHUNK_ATTEMPTS: u32 = 4;
/// Taille de chaque segment Xbox téléchargé en parallèle.
/// Des segments plus petits améliorent le pipeline : les workers restent moins longtemps
/// inactifs entre deux requêtes et la bande passante du CDN est mieux saturée.
const XBOX_RANGE_BYTES: u64 = 4 * 1024 * 1024;
/// Nombre de connexions parallèles pour les segments Xbox.
const XBOX_WORKERS: usize = 24;
/// Nombre de workers parallèles pour les chunks Epic.
const EPIC_CHUNK_WORKERS: usize = 16;
/// Nombre de clients HTTP (donc de connexions TCP) pour les chunks Epic. En HTTP/2, un seul
/// client multiplexe tout sur UNE connexion TCP, plafonnée par le CDN (~10 Mo/s) : on répartit
/// les workers sur plusieurs clients pour ouvrir plusieurs connexions.
const EPIC_CONNECTIONS: usize = 8;
/// Nombre de workers pour la vérification SHA-1 des fichiers Epic.
const VERIFY_WORKERS: usize = 8;
/// Bornes du nombre de requêtes simultanées, ajusté en direct selon le débit mesuré.
/// `XBOX_WORKERS` et `EPIC_CHUNK_WORKERS` ne sont plus que les valeurs de départ.
const ADAPTIVE_MIN_WORKERS: usize = 8;
const ADAPTIVE_MAX_WORKERS: usize = 48;
/// Pas d'augmentation à chaque essai et durée de maintien (en intervalles) après un essai raté.
const ADAPTIVE_STEP: usize = 4;
const ADAPTIVE_HOLD_INTERVALS: u32 = 5;
const ADAPTIVE_INTERVAL: Duration = Duration::from_secs(2);

/// Nombre de requêtes autorisées en même temps + octets reçus (pour mesurer le débit).
/// Les workers d'indice >= `target` restent en attente, sans quitter la file.
struct AdaptiveLimit {
    target: AtomicUsize,
    bytes: AtomicU64,
    /// Temps cumulé (ns) passé à écrire sur disque : sert à repérer un goulot disque.
    write_nanos: AtomicU64,
}

impl AdaptiveLimit {
    fn new(start: usize) -> Arc<Self> {
        Arc::new(Self {
            target: AtomicUsize::new(start.clamp(ADAPTIVE_MIN_WORKERS, ADAPTIVE_MAX_WORKERS)),
            bytes: AtomicU64::new(0),
            write_nanos: AtomicU64::new(0),
        })
    }

    fn target(&self) -> usize {
        self.target.load(Ordering::Relaxed)
    }

    fn add_bytes(&self, count: u64) {
        self.bytes.fetch_add(count, Ordering::Relaxed);
    }

    fn add_write_time(&self, elapsed: Duration) {
        self.write_nanos
            .fetch_add(elapsed.as_nanos() as u64, Ordering::Relaxed);
    }
}

enum TunerPhase {
    /// Laisse les nouvelles connexions monter en débit avant de mesurer.
    Settle(u32),
    Measure { ticks: u32, sum: f64 },
    Hold(u32),
}

/// Ajustement de la concurrence : on ne descend jamais sur un simple plateau (avec un CDN qui
/// limite chaque connexion, le débit est proportionnel au nombre de connexions : réduire sur
/// plateau ou sur une mesure bruitée bloquait le débit autour de 10 Mo/s). On monte par paliers ;
/// si un palier n'apporte pas au moins 5 % de débit, on revient au précédent, on le garde
/// quelques intervalles, puis on retente.
struct AdaptiveTuner {
    phase: TunerPhase,
    base: Option<f64>,
    fallback: usize,
    trial: bool,
}

impl AdaptiveTuner {
    fn new() -> Self {
        Self {
            phase: TunerPhase::Settle(2),
            base: None,
            fallback: ADAPTIVE_MIN_WORKERS,
            trial: false,
        }
    }

    fn climb(&mut self, current: usize) -> usize {
        if current >= ADAPTIVE_MAX_WORKERS {
            self.trial = false;
            self.phase = TunerPhase::Hold(ADAPTIVE_HOLD_INTERVALS);
            return current;
        }
        self.fallback = current;
        self.trial = true;
        self.phase = TunerPhase::Settle(1);
        (current + ADAPTIVE_STEP).min(ADAPTIVE_MAX_WORKERS)
    }

    /// `rate` : débit mesuré sur le dernier intervalle (octets/s). Renvoie la nouvelle limite.
    fn next(&mut self, current: usize, rate: f64) -> usize {
        if rate < 1.0 {
            // Pause ou coupure : rien ne bouge, la mesure repart de zéro à la reprise.
            self.phase = TunerPhase::Settle(2);
            self.base = None;
            self.trial = false;
            return current;
        }
        match std::mem::replace(&mut self.phase, TunerPhase::Settle(0)) {
            TunerPhase::Settle(ticks) => {
                self.phase = if ticks <= 1 {
                    TunerPhase::Measure { ticks: 0, sum: 0.0 }
                } else {
                    TunerPhase::Settle(ticks - 1)
                };
                current
            }
            TunerPhase::Measure { ticks, sum } => {
                let (ticks, sum) = (ticks + 1, sum + rate);
                if ticks < 2 {
                    self.phase = TunerPhase::Measure { ticks, sum };
                    return current;
                }
                let average = sum / f64::from(ticks);
                if self.trial {
                    let base = self.base.unwrap_or(0.0);
                    if average >= base * 1.05 {
                        self.base = Some(average);
                        self.climb(current)
                    } else {
                        self.trial = false;
                        self.phase = TunerPhase::Hold(ADAPTIVE_HOLD_INTERVALS);
                        self.fallback
                    }
                } else {
                    self.base = Some(average);
                    self.climb(current)
                }
            }
            TunerPhase::Hold(ticks) => {
                self.base = Some(self.base.map_or(rate, |base| base * 0.5 + rate * 0.5));
                if ticks <= 1 {
                    self.climb(current)
                } else {
                    self.phase = TunerPhase::Hold(ticks - 1);
                    current
                }
            }
        }
    }
}

async fn adaptive_controller(limit: Arc<AdaptiveLimit>) {
    let mut previous_bytes = 0u64;
    let mut tuner = AdaptiveTuner::new();
    let mut previous_write = 0u64;
    let mut until_log = 5u32;
    loop {
        tokio::time::sleep(ADAPTIVE_INTERVAL).await;
        let bytes = limit.bytes.load(Ordering::Relaxed);
        let rate = bytes.saturating_sub(previous_bytes) as f64 / ADAPTIVE_INTERVAL.as_secs_f64();
        previous_bytes = bytes;
        let next = tuner
            .next(limit.target(), rate)
            .clamp(ADAPTIVE_MIN_WORKERS, ADAPTIVE_MAX_WORKERS);
        limit.target.store(next, Ordering::Relaxed);
        let write = limit.write_nanos.load(Ordering::Relaxed);
        let disk_workers =
            write.saturating_sub(previous_write) as f64 / ADAPTIVE_INTERVAL.as_nanos() as f64;
        previous_write = write;
        until_log -= 1;
        if until_log == 0 {
            until_log = 5;
            download_log(format!(
                "Adaptive concurrency: {next} requests in flight at {:.1} MB/s (disk writes: {disk_workers:.1} workers busy)",
                rate / 1_000_000.0
            ));
        }
    }
}

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

/// Marque un fichier de sortie comme creux (« sparse ») sur Windows.
///
/// Les chunks Epic arrivent dans un ordre arbitraire et sont écrits directement à leur
/// offset final. Sur NTFS, la première écriture au-delà de la fin logique d'un fichier
/// ordinaire force Windows à remplir de zéros tout l'espace intermédiaire de façon
/// synchrone (plusieurs dizaines de secondes par Go), ce qui bloque le téléchargement et
/// fait chuter la vitesse affichée. Un fichier creux ne réserve que les blocs réellement
/// écrits ; les trous se relisent comme des zéros.
#[cfg(windows)]
fn mark_file_sparse(file: &File) -> io::Result<()> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn DeviceIoControl(
            device: *mut c_void,
            control_code: u32,
            in_buffer: *mut c_void,
            in_size: u32,
            out_buffer: *mut c_void,
            out_size: u32,
            bytes_returned: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
    }

    const FSCTL_SET_SPARSE: u32 = 0x0009_00C4;
    let mut returned = 0u32;
    let result = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            FSCTL_SET_SPARSE,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Runtime Tokio partagé par les transferts. Un client asynchrone multiplexe ses requêtes
/// HTTP/2 sur une seule connexion : plusieurs chunks restent en vol en même temps, alors que
/// `reqwest::blocking` attend la réponse de chaque appel avant de rendre la main.
fn download_runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    static RUNTIME: std::sync::OnceLock<Result<tokio::runtime::Runtime, String>> =
        std::sync::OnceLock::new();
    match RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .thread_name("download-io")
            .build()
            .map_err(|error| format!("Could not start the async download runtime: {error}"))
    }) {
        Ok(runtime) => Ok(runtime),
        Err(error) => Err(error.clone()),
    }
}

fn async_client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(timeout)
        .user_agent(crate::net::USER_AGENT)
        .http2_adaptive_window(true)
        .http2_keep_alive_interval(Duration::from_secs(20))
        .http2_keep_alive_timeout(Duration::from_secs(10))
        .http2_keep_alive_while_idle(true)
        .tcp_nodelay(true)
        .build()
        .map_err(|error| format!("Could not create HTTP client: {error}"))
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
    /// Progress of a Ubisoft installer expanded while the download is still running.
    pub installer_percent: Option<u64>,
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

    /// Attente non bloquante pour le code async : bloquer un thread du runtime pendant une
    /// pause empêcherait les requêtes encore en vol d'avancer.
    async fn wait_until_resumed_async(&self) -> bool {
        loop {
            match self.get() {
                ControlState::Running => return true,
                ControlState::Paused => tokio::time::sleep(Duration::from_millis(100)).await,
                ControlState::Cancelled => return false,
            }
        }
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
    /// A Ubisoft installer is being expanded while the download keeps running.
    InstallerProgress {
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
    /// Dernier contrôle d'existence des installations terminées (appel disque coûteux).
    last_prune: Instant,
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
            last_prune: Instant::now(),
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
            let received = resumable_xbox_bytes(&partial);
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
                installer_percent: None,
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
                installer_percent: None,
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
            installer_percent: None,
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
            installer_percent: None,
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
            if result.as_ref().is_err_and(|error| {
                !error.starts_with("Epic installation extraction failed; keeping downloaded files:")
            }) {
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
        let mut respawn: Vec<u64> = Vec::new();
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
                            Err(error) if error == "paused" => {
                                // Le thread Xbox s'arrête sur une pause : la reprise doit en
                                // relancer un (sinon le téléchargement reste bloqué).
                                if entry.product_id.is_some() {
                                    entry.restored = true;
                                    if entry.control.get() == ControlState::Running {
                                        respawn.push(id);
                                    }
                                }
                                DownloadStatus::Paused
                            }
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
                        // Le débit du téléchargement n'a plus de sens ici (vitesse/ETA périmés).
                        entry.speed = 0;
                        entry.status = match entry.control.get() {
                            ControlState::Running => DownloadStatus::Extracting,
                            ControlState::Paused => DownloadStatus::Paused,
                            ControlState::Cancelled => DownloadStatus::Cancelled,
                        };
                        changed = true;
                    }
                }
                Ok(DownloadEvent::InstallerProgress { id, percent }) => {
                    if let Some(entry) = self.entries.iter_mut().find(|item| item.id == id) {
                        // The transfer keeps its own bar; this one reports the installer
                        // being expanded in the background.
                        entry.installer_percent = Some(percent.min(100));
                        changed = true;
                    }
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        // Reprise demandée pendant que le thread Xbox terminait sa pause : on en relance un.
        for id in respawn {
            self.resume(id);
            changed = true;
        }
        if changed {
            self.persist_downloads();
        }
        changed |= self.prune_missing_completed();
        changed
    }

    /// Retire les installations terminées dont le dossier a disparu. Un `exists()` par entrée
    /// à chaque frame coûte cher (antivirus, disque réseau ou en veille) : on espace les
    /// contrôles de 2 s, ce qui suffit largement pour détecter une suppression manuelle.
    fn prune_missing_completed(&mut self) -> bool {
        const PRUNE_INTERVAL: Duration = Duration::from_secs(2);
        if self.last_prune.elapsed() < PRUNE_INTERVAL {
            return false;
        }
        self.last_prune = Instant::now();
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
/// Journal de reprise d'une installation Epic (fichier caché dans le dossier du jeu) : chunks
/// déjà écrits (`C <guid>`) et fichiers déjà vérifiés (`V <chemin>`). Il permet de reprendre
/// après une fermeture de l'appli sans tout retélécharger ni tout revérifier.
struct EpicJournal {
    path: PathBuf,
    writer: Mutex<Option<std::io::BufWriter<File>>>,
}

type EpicJournalState = (
    std::collections::HashSet<[u32; 4]>,
    std::collections::HashSet<String>,
);

const EPIC_JOURNAL_NAME: &str = ".epic-resume.journal";

impl EpicJournal {
    fn open(path: PathBuf, key: &str, resume: bool) -> Self {
        let file = if resume {
            OpenOptions::new()
                .append(true)
                .open(&path)
                .and_then(|mut file| {
                    // Une dernière ligne tronquée par une fermeture brutale est isolée.
                    writeln!(file)?;
                    Ok(file)
                })
        } else {
            File::create(&path).and_then(|mut file| {
                writeln!(file, "EPICJ1 {key}")?;
                Ok(file)
            })
        };
        Self {
            path,
            writer: Mutex::new(file.ok().map(std::io::BufWriter::new)),
        }
    }

    fn write_line(&self, line: &str, flush: bool) {
        let mut guard = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(writer) = guard.as_mut() {
            let mut result = writeln!(writer, "{line}");
            if result.is_ok() && flush {
                result = writer.flush().and_then(|()| writer.get_ref().sync_data());
            }
            if result.is_err() {
                *guard = None;
            }
        }
    }

    fn record_chunk(&self, guid: &[u32; 4]) {
        self.write_line(
            &format!(
                "C {:08x}{:08x}{:08x}{:08x}",
                guid[0], guid[1], guid[2], guid[3]
            ),
            false,
        );
    }

    fn record_verified(&self, path: &str) {
        self.write_line(&format!("V {path}"), true);
    }

    fn flush(&self) {
        let mut guard = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(writer) = guard.as_mut()
            && writer
                .flush()
                .and_then(|()| writer.get_ref().sync_data())
                .is_err()
        {
            *guard = None;
        }
    }

    /// Supprime le journal (installation terminée, ou état à ne plus réutiliser).
    fn remove(&self) {
        *self.writer.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let _ = fs::remove_file(&self.path);
    }
}

/// Relit un journal de reprise ; `None` si absent ou s'il date d'un autre manifest.
fn load_epic_journal(path: &Path, key: &str) -> Option<EpicJournalState> {
    let raw = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&raw);
    // Dernière ligne éventuellement incomplète : ignorée.
    let complete = &text[..text.rfind('\n')?];
    let mut lines = complete.lines();
    if lines.next()? != format!("EPICJ1 {key}") {
        return None;
    }
    let mut chunks = std::collections::HashSet::new();
    let mut verified = std::collections::HashSet::new();
    for line in lines {
        if let Some(hex) = line.strip_prefix("C ") {
            if hex.len() == 32 && hex.is_ascii() {
                let part = |index: usize| u32::from_str_radix(&hex[index * 8..index * 8 + 8], 16);
                if let (Ok(a), Ok(b), Ok(c), Ok(d)) = (part(0), part(1), part(2), part(3)) {
                    chunks.insert([a, b, c, d]);
                }
            }
        } else if let Some(name) = line.strip_prefix("V ") {
            verified.insert(name.to_owned());
        }
    }
    Some((chunks, verified))
}

/// Poignées de fichiers de sortie réutilisées entre les chunks Epic. Un jeu compte des milliers
/// de chunks : rouvrir le fichier cible pour chacun coûte un appel système (et un passage
/// d'antivirus sous Windows) par écriture, exécuté sur le thread qui reçoit aussi la progression.
#[derive(Default)]
struct OutputHandles {
    files: std::collections::HashMap<PathBuf, File>,
}

impl OutputHandles {
    /// Nombre maximal de fichiers gardés ouverts (au-delà, on repart de zéro).
    const MAX_OPEN: usize = 64;

    fn write_at(&mut self, path: &Path, offset: u64, data: &[u8]) -> io::Result<()> {
        if !self.files.contains_key(path) {
            if self.files.len() >= Self::MAX_OPEN {
                self.files.clear();
            }
            let file = OpenOptions::new().write(true).open(path)?;
            self.files.insert(path.to_path_buf(), file);
        }
        let Some(file) = self.files.get_mut(path) else {
            return Err(io::Error::other("output handle missing"));
        };
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(data)
    }

    /// Force l'écriture disque des fichiers ouverts avant de valider le journal.
    fn sync_all(&self) {
        for file in self.files.values() {
            let _ = file.sync_data();
        }
    }

    /// Ferme la poignée d'un fichier terminé (avant qu'un autre thread ne le lise ou le supprime).
    fn forget(&mut self, path: &Path) {
        self.files.remove(path);
    }
}

/// Vérifie les SHA-1 des fichiers reconstruits avec quelques threads : lire un seul fichier à
/// la fois laisse un SSD presque au repos et la vérification d'un gros jeu dure des minutes.
fn verify_epic_files(
    files: &[(PathBuf, Vec<u8>)],
    total_bytes: u64,
    download_total: u64,
    already_verified: &std::collections::HashSet<String>,
    journal: &EpicJournal,
    control: &Control,
    tx: &mpsc::Sender<DownloadEvent>,
    id: u64,
) -> Result<(), String> {
    use sha1::Digest;
    let next = AtomicUsize::new(0);
    let verified = AtomicU64::new(0);
    let last_percent = AtomicU64::new(0);
    let started = Instant::now();
    let failure = Mutex::new(None::<String>);
    let fail = |message: String| {
        let mut slot = failure.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(message);
        }
    };
    let failed = || {
        failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    };
    let workers = VERIFY_WORKERS.min(files.len()).max(1);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let tx = tx.clone();
            let (next, verified, last_percent) = (&next, &verified, &last_percent);
            let (fail, failed) = (&fail, &failed);
            scope.spawn(move || {
                let mut buffer = vec![0u8; 1024 * 1024];
                while !failed() {
                    let Some((path, expected_sha1)) =
                        files.get(next.fetch_add(1, Ordering::Relaxed))
                    else {
                        break;
                    };
                    if !path.exists() && is_upc_volume(path) {
                        // Volume expanded slice by slice (each slice SHA-1 checked) and removed.
                        continue;
                    }
                    if already_verified.contains(path.to_string_lossy().as_ref()) && path.exists() {
                        // Fichier déjà vérifié avant l'interruption.
                        verified.fetch_add(
                            fs::metadata(path).map(|meta| meta.len()).unwrap_or(0),
                            Ordering::Relaxed,
                        );
                        continue;
                    }
                    if !control.wait_until_resumed() {
                        fail("cancelled".into());
                        break;
                    }
                    let mut input = match File::open(path) {
                        Ok(input) => input,
                        Err(error) => {
                            fail(error.to_string());
                            break;
                        }
                    };
                    let mut file_hash = sha1::Sha1::new();
                    loop {
                        let read = match input.read(&mut buffer) {
                            Ok(read) => read,
                            Err(error) => {
                                fail(error.to_string());
                                return;
                            }
                        };
                        if read == 0 {
                            break;
                        }
                        file_hash.update(&buffer[..read]);
                        let done = verified
                            .fetch_add(read as u64, Ordering::Relaxed)
                            .saturating_add(read as u64);
                        let percent = done
                            .saturating_mul(100)
                            .checked_div(total_bytes)
                            .unwrap_or(100)
                            .min(100);
                        if last_percent.fetch_max(percent, Ordering::Relaxed) < percent {
                            // Dernière tranche (10 %) de la barre unique : octets « virtuels »
                            // au-delà du téléchargement réel.
                            let extra = download_total / 9;
                            let received = download_total
                                .saturating_add((u128::from(extra) * u128::from(done)
                                    / u128::from(total_bytes.max(1)))
                                    as u64);
                            let speed = (done as f64 / started.elapsed().as_secs_f64().max(0.001))
                                as u64;
                            publish_progress(
                                &tx,
                                id,
                                received,
                                download_total.saturating_add(extra),
                                speed,
                            );
                        }
                    }
                    if file_hash.finalize().as_slice() != expected_sha1.as_slice() {
                        fail(format!("File hash verification failed: {}", path.display()));
                        break;
                    }
                    journal.record_verified(&path.to_string_lossy());
                }
            });
        }
    });
    match failure.into_inner().unwrap_or_else(PoisonError::into_inner) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// File de téléchargement : fichiers triés par `ranks` (stable), chunks dans l'ordre des parties,
/// chaque chunk (éventuellement partagé entre fichiers) une seule fois.
fn order_epic_chunks(
    files: &[EpicFile],
    by_guid: &std::collections::HashMap<[u32; 4], EpicChunk>,
    ranks: &[u8],
) -> VecDeque<EpicChunk> {
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by_key(|&index| ranks[index]);
    let mut seen = std::collections::HashSet::with_capacity(by_guid.len());
    let mut queue = VecDeque::with_capacity(by_guid.len());
    for index in order {
        for part in &files[index].parts {
            if seen.insert(part.guid)
                && let Some(chunk) = by_guid.get(&part.guid)
            {
                queue.push_back(chunk.clone());
            }
        }
    }
    for chunk in by_guid.values() {
        if seen.insert(chunk.guid) {
            queue.push_back(chunk.clone());
        }
    }
    queue
}

struct EpicChunkTarget {
    path: PathBuf,
    output_offset: u64,
    chunk_offset: u32,
    size: u32,
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

enum EpicWorkerMessage {
    Progress {
        guid: [u32; 4],
        downloaded: u64,
        logical: u64,
    },
    Finished {
        guid: [u32; 4],
        size: u64,
        result: Result<Vec<u8>, String>,
    },
}

fn run_epic_download(
    id: u64,
    game: Game,
    root: PathBuf,
    control: Arc<Control>,
    tx: mpsc::Sender<DownloadEvent>,
) -> Result<(), String> {
    use sha1::Digest;
    let runtime = download_runtime()?;
    let client = runtime.block_on(async { async_client(Duration::from_secs(180)) })?;
    let chunk_clients = runtime.block_on(async {
        (0..EPIC_CONNECTIONS)
            .map(|_| async_client(Duration::from_secs(180)))
            .collect::<Result<Vec<_>, String>>()
    })?;
    let manifest_url = if game.manifest_url.starts_with("http") {
        game.manifest_url.clone()
    } else {
        format!("{}{}", crate::catalog::EPIC_API_BASE, game.manifest_url)
    };
    let bytes = runtime.block_on(async {
        client
            .get(&manifest_url)
            .send()
            .await
            .map_err(|e| format!("Epic manifest request failed: {e}"))?
            .error_for_status()
            .map_err(|e| format!("Epic manifest request failed: {e}"))?
            .bytes()
            .await
            .map_err(|e| e.to_string())
    })?;
    let (chunks, files, feature, prereq) = parse_epic_manifest(&bytes)?;
    let by_guid: std::collections::HashMap<_, _> =
        chunks.into_iter().map(|c| (c.guid, c)).collect();
    // Jeu Ubisoft : le build Epic embarque le paquet « disc » et l'installeur Ubisoft déclaré
    // comme prérequis. Le launcher Epic exécute ce prérequis (Ubisoft Connect installe alors
    // depuis les fichiers locaux). On fait pareil, sauf si le réglage est coupé ou si le
    // prérequis n'est pas dans le build : l'extraction directe reste alors le comportement.
    let has_ubisoft_disc = ["uplay_install.disc", "uplay_install.manifest"]
        .iter()
        .all(|name| {
            files
                .iter()
                .any(|file| manifest_file_name(&file.name).eq_ignore_ascii_case(name))
        });
    let prereq_in_build = !prereq.path.trim().is_empty()
        && files
            .iter()
            .any(|file| same_manifest_path(&file.name, &prereq.path));
    if has_ubisoft_disc {
        download_log(format!(
            "Epic Ubisoft build: prerequisite name={:?} path={:?} args={:?} in_build={prereq_in_build}",
            prereq.name, prereq.path, prereq.args
        ));
    }
    let connect_mode =
        has_ubisoft_disc && prereq_in_build && crate::settings::ubisoft_connect_enabled();
    let bytes_total: u64 = by_guid.values().map(|c| c.size).sum();
    if bytes_total == 0 {
        return Err("Epic manifest contains no chunks.".into());
    }
    // Une seule barre, comme le launcher Epic : le téléchargement occupe les 90 premiers %,
    // la vérification des fichiers les 10 derniers.
    let display_total = bytes_total.saturating_add(bytes_total / 9);
    // Reprise : le journal n'est réutilisé que pour le même manifest, et si les fichiers du jeu
    // sont toujours là (un volume Ubisoft déjà extrait puis supprimé est normal).
    let manifest_key: String = {
        use sha1::Digest;
        sha1::Sha1::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    };
    let journal_path = root.join(EPIC_JOURNAL_NAME);
    let loaded = load_epic_journal(&journal_path, &manifest_key).filter(|(done, _)| {
        files.iter().all(|file| {
            let destination = root.join(&file.name);
            destination.exists()
                || (is_upc_volume(&destination)
                    && file.parts.iter().all(|part| done.contains(&part.guid)))
        })
    });
    let resumed = loaded.is_some();
    let (done_chunks, verified_files) = loaded.unwrap_or_default();
    if resumed {
        download_log(format!(
            "Epic install resumed from journal: {} chunks already written, {} files already verified",
            done_chunks.len(),
            verified_files.len()
        ));
    }
    let mut received_chunks = 0u64;
    let mut chunk_targets = std::collections::HashMap::<[u32; 4], Vec<EpicChunkTarget>>::new();
    let mut files_to_verify = Vec::<(PathBuf, Vec<u8>)>::with_capacity(files.len());
    let mut total_output_bytes = 0u64;
    // Ubisoft volumes can be expanded as soon as every chunk that feeds them has
    // been written, instead of waiting for the whole download to finish.
    let mut volume_chunks = std::collections::HashMap::<PathBuf, usize>::new();
    let mut meta_paths = std::collections::HashSet::<PathBuf>::new();
    let mut meta_outstanding = 0usize;

    // Create final output files up front and record where each chunk belongs.
    // Chunks are decompressed in memory and written directly to these locations.
    for file in &files {
        let relative = Path::new(&file.name);
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::Prefix(_)
                )
            })
        {
            return Err(format!("Unsafe path in Epic manifest: {}", file.name));
        }
        let destination = root.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let mut output_offset = 0u64;
        for part in &file.parts {
            if !by_guid.contains_key(&part.guid) {
                return Err(format!(
                    "Epic manifest references an unknown chunk for {}",
                    file.name
                ));
            }
            chunk_targets
                .entry(part.guid)
                .or_default()
                .push(EpicChunkTarget {
                    path: destination.clone(),
                    output_offset,
                    chunk_offset: part.offset,
                    size: part.size,
                });
            output_offset = output_offset.saturating_add(part.size as u64);
        }
        let name_only = destination.file_name().and_then(|name| name.to_str());
        // Les volumes sont toujours expansés : le prérequis Ubisoft déclaré par Epic n'est que
        // l'installeur du client Ubisoft Connect, pas celui du jeu. Il ne crée aucun fichier de
        // jeu, donc s'en remettre à lui laissait une installation vide.
        if is_upc_volume(&destination) {
            volume_chunks.insert(destination.clone(), file.parts.len());
        } else if matches!(
            name_only,
            Some("uplay_install.disc") | Some("uplay_install.manifest")
        ) {
            meta_paths.insert(destination.clone());
            meta_outstanding += file.parts.len();
        }
        if file.parts.is_empty() && sha1::Sha1::digest([]).as_slice() != file.sha1.as_slice() {
            return Err(format!(
                "Unsupported Epic manifest entry without chunk data: {}",
                file.name
            ));
        }
        if !resumed {
            let output = File::create(&destination).map_err(|error| {
                format!("Could not create {}: {error}", destination.display())
            })?;
            // Chunks are written at their final offsets in whatever order they arrive, so the
            // file must not pay for zero-filling the gaps between them (see `mark_file_sparse`).
            // On other platforms unwritten regions are sparse by default.
            #[cfg(windows)]
            if let Err(error) = mark_file_sparse(&output) {
                download_log(format!(
                    "Could not mark {} as sparse ({error}); chunk writes may be slow",
                    destination.display()
                ));
            }
            drop(output);
        }
        // En reprise, les fichiers existants sont conservés tels quels (sans troncature).
        total_output_bytes = total_output_bytes.saturating_add(output_offset);
        files_to_verify.push((destination, file.sha1.clone()));
    }

    // Ordre de téléchargement : métadonnées Ubisoft d'abord, puis les volumes un par un, puis
    // le reste. Avant, la file suivait l'ordre aléatoire d'une HashMap : les chunks de tous les
    // volumes arrivaient mélangés, aucun volume n'était complet avant la toute fin, et
    // l'extraction « incrémentale » n'avait en pratique rien à extraire pendant le téléchargement.
    let ranks: Vec<u8> = files
        .iter()
        .map(|file| {
            let destination = root.join(&file.name);
            if meta_paths.contains(&destination) {
                0
            } else if volume_chunks.contains_key(&destination) {
                1
            } else {
                2
            }
        })
        .collect();
    let mut pending = order_epic_chunks(&files, &by_guid, &ranks);
    let _ = fs::create_dir_all(&root);
    let journal = Arc::new(EpicJournal::open(journal_path, &manifest_key, resumed));
    if resumed {
        // Chunks déjà écrits : ni retéléchargés, ni recomptés dans les volumes/métadonnées Ubisoft.
        pending.retain(|chunk| !done_chunks.contains(&chunk.guid));
        for guid in &done_chunks {
            if let Some(targets) = chunk_targets.get(guid) {
                for target in targets {
                    if let Some(left) = volume_chunks.get_mut(&target.path) {
                        *left = left.saturating_sub(1);
                    }
                    if meta_paths.contains(&target.path) {
                        meta_outstanding = meta_outstanding.saturating_sub(1);
                    }
                }
            }
            received_chunks =
                received_chunks.saturating_add(by_guid.get(guid).map_or(0, |chunk| chunk.size));
        }
        received_chunks = received_chunks.min(bytes_total);
        publish_progress(&tx, id, received_chunks, display_total, 0);
    }

    // Epic manifests can contain thousands of small chunks. Fetching them one at a time
    // leaves the connection idle between requests, so use a bounded worker pool.
    let pending_count = pending.len();
    // Start measuring only once the output files are prepared: the setup time must not
    // count as download time, otherwise the first reported speed is far too low.
    let mut meter = SpeedMeter::new();
    let installer_dir = root.join("Installer");
    let (extract_tx, extract_rx) = mpsc::channel::<ExtractRequest>();
    let extraction = {
        let installer_dir = installer_dir.clone();
        let output_dir = root.clone();
        let tx = tx.clone();
        std::thread::spawn(move || {
            run_incremental_extraction(extract_rx, installer_dir, output_dir, tx, id)
        })
    };
    // A volume can only be expanded once the disc index and manifest are complete.
    let mut extraction_ready = !meta_paths.is_empty() && meta_outstanding == 0;
    let mut waiting_volumes = Vec::<PathBuf>::new();
    if volume_chunks.is_empty() {
        let _ = extract_tx.send(ExtractRequest::Stop);
    } else if extraction_ready {
        let _ = extract_tx.send(ExtractRequest::Ready);
        for (path, pending) in &volume_chunks {
            // Un volume déjà extrait (donc supprimé) avant l'interruption n'est pas rejoué.
            if *pending == 0 && path.exists() {
                waiting_volumes.push(path.clone());
            }
        }
        for path in waiting_volumes.drain(..) {
            let _ = extract_tx.send(ExtractRequest::Volume(path));
        }
    }
    let stream_journal = Arc::clone(&journal);
    let stream_control = Arc::clone(&control);
    let stream_tx = tx.clone();
    let stream_extract_tx = extract_tx.clone();
    let cdns = game.cdn_bases.clone();
    let stream_result = if pending_count > 0 {
        let jobs = Arc::new(Mutex::new(pending));
        let worker_count = ADAPTIVE_MAX_WORKERS.min(pending_count);
        let limit = AdaptiveLimit::new(EPIC_CHUNK_WORKERS);
        let (result_tx, mut result_rx) =
            tokio::sync::mpsc::channel::<EpicWorkerMessage>(worker_count * 8);
        runtime.block_on(async move {
            let journal = stream_journal;
            let control = stream_control;
            let tx = stream_tx;
            let extract_tx = stream_extract_tx;
            let mut workers = tokio::task::JoinSet::new();
            for index in 0..worker_count {
                workers.spawn(epic_chunk_worker(
                    index,
                    Arc::clone(&limit),
                    Arc::clone(&jobs),
                    chunk_clients[index % chunk_clients.len()].clone(),
                    cdns.clone(),
                    feature,
                    Arc::clone(&control),
                    result_tx.clone(),
                ));
            }
            workers.spawn(adaptive_controller(Arc::clone(&limit)));
            drop(result_tx);
            download_log(format!(
                "Epic chunks: adaptive concurrency, up to {worker_count} requests (HTTP version is logged on the first response)"
            ));
            let mut first_error = None;
            let mut handles = OutputHandles::default();
            let mut last_journal_flush = Instant::now();
            let mut completed_chunks = 0usize;
            let mut in_flight = std::collections::HashMap::<[u32; 4], (u64, u64)>::new();
            while first_error.is_none() && completed_chunks < pending_count {
                let Some(message) = result_rx.recv().await else {
                    return Err("Epic chunk workers stopped unexpectedly.".to_owned());
                };
                let (guid, size, result) = match message {
                    EpicWorkerMessage::Progress {
                        guid,
                        downloaded,
                        logical,
                    } => {
                        let state = in_flight.entry(guid).or_insert((0, 0));
                        if downloaded < state.0 {
                            state.0 = 0;
                            state.1 = 0;
                        }
                        meter.add(downloaded.saturating_sub(state.0));
                        state.0 = downloaded;
                        state.1 = logical;
                        let partial = in_flight.values().map(|(_, bytes)| *bytes).sum::<u64>();
                        publish_progress(
                            &tx,
                            id,
                            received_chunks.saturating_add(partial).min(bytes_total),
                            display_total,
                            meter.speed(),
                        );
                        continue;
                    }
                    EpicWorkerMessage::Finished { guid, size, result } => {
                        completed_chunks += 1;
                        in_flight.remove(&guid);
                        (guid, size, result)
                    }
                };
                match result {
                    Ok(decoded) => {
                        if first_error.is_none()
                            && let Some(targets) = chunk_targets.get(&guid)
                        {
                            for target in targets {
                                if !control.wait_until_resumed_async().await {
                                    first_error = Some("cancelled".to_owned());
                                    break;
                                }
                                let start = target.chunk_offset as usize;
                                let end = start
                                    .checked_add(target.size as usize)
                                    .filter(|end| *end <= decoded.len());
                                let Some(end) = end else {
                                    first_error = Some("Epic chunk part is out of range.".into());
                                    break;
                                };
                                let write_result = handles.write_at(
                                    &target.path,
                                    target.output_offset,
                                    &decoded[start..end],
                                );
                                if let Err(error) = write_result {
                                    first_error = Some(format!(
                                        "Could not write {}: {error}",
                                        target.path.display()
                                    ));
                                    break;
                                }
                                if let Some(pending) = volume_chunks.get_mut(&target.path) {
                                    *pending = pending.saturating_sub(1);
                                    if *pending == 0 {
                                        handles.forget(&target.path);
                                        if extraction_ready {
                                            let _ = extract_tx
                                                .send(ExtractRequest::Volume(target.path.clone()));
                                        } else {
                                            waiting_volumes.push(target.path.clone());
                                        }
                                    }
                                }
                                if meta_paths.contains(&target.path) {
                                    meta_outstanding = meta_outstanding.saturating_sub(1);
                                    if meta_outstanding == 0 && !extraction_ready {
                                        for path in &meta_paths {
                                            handles.forget(path);
                                        }
                                        extraction_ready = true;
                                        let _ = extract_tx.send(ExtractRequest::Ready);
                                        for path in waiting_volumes.drain(..) {
                                            let _ = extract_tx.send(ExtractRequest::Volume(path));
                                        }
                                    }
                                }
                            }
                        }
                        if first_error.is_none() {
                            journal.record_chunk(&guid);
                            if last_journal_flush.elapsed() >= Duration::from_secs(2) {
                                handles.sync_all();
                                journal.flush();
                                last_journal_flush = Instant::now();
                            }
                        }
                        received_chunks = received_chunks.saturating_add(size);
                        publish_progress(&tx, id, received_chunks, display_total, meter.speed());
                    }
                    Err(error) => {
                        if first_error.is_none() {
                            first_error = Some(error);
                        }
                    }
                }
            }
            // Pause longue, erreur ou fermeture : l'état écrit est rendu durable avant de sortir.
            handles.sync_all();
            journal.flush();
            // Première erreur (ou annulation) : on n'attend pas la fin de la file.
            workers.abort_all();
            while workers.join_next().await.is_some() {}
            if let Some(error) = first_error {
                Err(error)
            } else {
                Ok(())
            }
        })
    } else {
        Ok(())
    };

    let _ = extract_tx.send(ExtractRequest::Stop);
    drop(extract_tx);
    let extraction = extraction
        .join()
        .map_err(|_| "Ubisoft extraction thread panicked.".to_owned())?;
    // A download error must not leave the extraction thread writing into a folder the
    // caller is about to delete, so it is stopped and joined before it is reported.
    stream_result?;
    let ExtractionOutcome {
        unpacker,
        error: extraction_error,
    } = extraction;
    if let Some(error) = extraction_error {
        download_log(format!(
            "Epic Ubisoft installer extraction failed in {}: {error}",
            root.display()
        ));
        return Err(format!(
            "Epic installation extraction failed; keeping downloaded files: {error}"
        ));
    }

    // Verify the reconstructed files after streaming completes. This reads them in
    // bounded memory and reports the final integrity-check phase in the UI.
    if let Err(error) = verify_epic_files(
        &files_to_verify,
        total_output_bytes,
        bytes_total,
        &verified_files,
        &journal,
        &control,
        &tx,
        id,
    ) {
        if error != "cancelled" {
            // Fichier corrompu : la prochaine tentative repart d'un état propre.
            journal.remove();
        }
        return Err(error);
    }
    let _ = tx.send(DownloadEvent::ExtractionProgress { id, percent: 100 });
    // Le prérequis Ubisoft du manifest n'est que l'installeur du client Ubisoft Connect
    // (`UplayInstaller.exe`, produit « Uplay 85.0 installer »). Il ne dépose aucun fichier de
    // jeu : on l'exécute pour installer le client, mais c'est toujours l'extraction directe des
    // volumes `.upc` qui installe le jeu. Il tourne avant `finish()` car celui-ci supprime le
    // dossier `Installer` qui le contient.
    let ubisoft_connect = if connect_mode {
        download_log(format!(
            "Epic Ubisoft build: installing Ubisoft Connect with {:?} {:?} in {}",
            prereq.path,
            prereq.args,
            root.display()
        ));
        Some(run_ubisoft_prereq(&root, &prereq, &control))
    } else {
        None
    };

    let unpacked = if let Some(prepared) = unpacker {
        // The disc index and manifest have just been verified and every volume was
        // expanded slice by slice, so the consumed installer can be removed now.
        if let Err(error) = prepared.finish() {
            download_log(format!(
                "Epic Ubisoft installer extraction failed in {}: {error}",
                root.display()
            ));
            return Err(format!(
                "Epic installation extraction failed; keeping downloaded files: {error}"
            ));
        }
        true
    } else {
        match crate::upc::unpack_if_present(&installer_dir, &root, |percent| {
            let _ = tx.send(DownloadEvent::ExtractionProgress { id, percent });
        }) {
            Ok(unpacked) => unpacked,
            Err(error) => {
                download_log(format!(
                    "Epic Ubisoft installer extraction failed in {}: {error}",
                    root.display()
                ));
                return Err(format!(
                    "Epic installation extraction failed; keeping downloaded files: {error}"
                ));
            }
        }
    };
    if unpacked {
        let _ = tx.send(DownloadEvent::InstallerProgress { id, percent: 100 });
        download_log(format!(
            "Epic Ubisoft installer unpacked directly into {}",
            root.display()
        ));
    }

    match ubisoft_connect {
        Some(Ok(())) => download_log("Ubisoft Connect installer finished"),
        Some(Err(error)) if error == "cancelled" => return Err(error),
        Some(Err(error)) => download_log(format!(
            "Ubisoft Connect installer could not complete ({error}); the game files are still installed"
        )),
        None => {}
    }
    // Installation terminée : plus rien à reprendre.
    journal.remove();
    Ok(())
}

/// Dernier segment d'un chemin de manifest (séparateurs `\\` ou `/`).
fn manifest_file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// Compare deux chemins de manifest sans tenir compte de la casse ni du type de séparateur.
fn same_manifest_path(a: &str, b: &str) -> bool {
    let normalize = |path: &str| {
        path.trim()
            .replace('\\', "/")
            .trim_start_matches('/')
            .to_ascii_lowercase()
    };
    normalize(a) == normalize(b)
}

/// Lance le prérequis déclaré par le manifest (l'installeur Ubisoft) comme le fait le launcher
/// Epic : exécutable relatif au dossier du jeu, arguments du manifest, dossier du jeu comme
/// répertoire courant. Attend sa fin ; une élévation UAC est demandée si nécessaire.
fn run_ubisoft_prereq(root: &Path, prereq: &EpicPrereq, control: &Control) -> Result<(), String> {
    let relative = prereq.path.trim().replace('\\', "/");
    let relative_path = Path::new(&relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(format!("unsafe prerequisite path {:?}", prereq.path));
    }
    let exe = root.join(relative_path);
    if !exe.is_file() {
        return Err(format!("prerequisite not found: {}", exe.display()));
    }
    let args = prereq.args.trim();
    let mut command = Command::new(&exe);
    command.current_dir(root);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        if !args.is_empty() {
            command.raw_arg(args);
        }
    }
    #[cfg(not(windows))]
    command.args(args.split_whitespace());
    let mut child = match command.spawn() {
        Ok(child) => child,
        // ERROR_ELEVATION_REQUIRED : l'installeur demande les droits administrateur.
        Err(error) if error.raw_os_error() == Some(740) => spawn_elevated(&exe, args, root)?,
        Err(error) => return Err(format!("could not start {}: {error}", exe.display())),
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("installer exited with {status}")),
            Ok(None) => {
                if control.get() == ControlState::Cancelled {
                    let _ = child.kill();
                    return Err("cancelled".into());
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(error) => return Err(format!("could not wait for the installer: {error}")),
        }
    }
}

/// Relance l'exécutable avec élévation (UAC) via PowerShell et propage son code de sortie.
fn spawn_elevated(exe: &Path, args: &str, dir: &Path) -> Result<std::process::Child, String> {
    let quote = |text: &str| text.replace('\'', "''");
    let mut script = format!(
        "$p = Start-Process -FilePath '{}' -WorkingDirectory '{}' -Verb RunAs -Wait -PassThru",
        quote(&exe.to_string_lossy()),
        quote(&dir.to_string_lossy())
    );
    if !args.is_empty() {
        script.push_str(&format!(" -ArgumentList '{}'", quote(args)));
    }
    script.push_str("; exit $p.ExitCode");
    let mut command = Command::new("powershell");
    command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
        .spawn()
        .map_err(|error| format!("could not start the elevated installer: {error}"))
}

/// Requests sent from the download loop to the thread expanding Ubisoft volumes.
enum ExtractRequest {
    /// The disc index and manifest are complete: volumes can now be expanded.
    Ready,
    Volume(PathBuf),
    Stop,
}

struct ExtractionOutcome {
    unpacker: Option<crate::upc::UbisoftUnpacker>,
    error: Option<String>,
}

/// True for the Ubisoft package volumes expanded while the download is running.
fn is_upc_volume(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("upc"))
}

/// Expands each Ubisoft volume as soon as the download has written it, so extraction
/// overlaps the remaining download instead of starting only once it is finished.
fn run_incremental_extraction(
    requests: mpsc::Receiver<ExtractRequest>,
    installer_dir: PathBuf,
    output_dir: PathBuf,
    tx: mpsc::Sender<DownloadEvent>,
    id: u64,
) -> ExtractionOutcome {
    let mut unpacker: Option<crate::upc::UbisoftUnpacker> = None;
    let mut waiting = Vec::<PathBuf>::new();
    let mut error = None;
    for request in requests {
        match request {
            ExtractRequest::Ready => {
                if unpacker.is_some() {
                    continue;
                }
                match crate::upc::UbisoftUnpacker::prepare(&installer_dir, &output_dir) {
                    Ok(Some(mut prepared)) => {
                        let _ = tx.send(DownloadEvent::InstallerProgress { id, percent: 0 });
                        for path in waiting.drain(..) {
                            let mut report = |percent: u64| {
                                let _ = tx.send(DownloadEvent::InstallerProgress { id, percent });
                            };
                            if let Err(message) = prepared.extract_volume(&path, &mut report) {
                                error = Some(message);
                                break;
                            }
                        }
                        unpacker = Some(prepared);
                    }
                    Ok(None) => {}
                    Err(message) => error = Some(message),
                }
            }
            ExtractRequest::Volume(path) => match unpacker.as_mut() {
                Some(prepared) => {
                    let mut report = |percent: u64| {
                        let _ = tx.send(DownloadEvent::InstallerProgress { id, percent });
                    };
                    if let Err(message) = prepared.extract_volume(&path, &mut report) {
                        error = Some(message);
                    }
                }
                None => waiting.push(path),
            },
            ExtractRequest::Stop => break,
        }
        if error.is_some() {
            break;
        }
    }
    if error.is_some() {
        // Keep the consumed installer so the failure is visible with the other files.
        unpacker = None;
    }
    ExtractionOutcome { unpacker, error }
}

/// Prérequis déclaré par le manifest Epic (exécuté par le launcher Epic après l'installation).
#[derive(Default)]
struct EpicPrereq {
    name: String,
    path: String,
    args: String,
}

type ParsedEpicManifest = (Vec<EpicChunk>, Vec<EpicFile>, u32, EpicPrereq);

fn parse_epic_manifest(raw: &[u8]) -> Result<ParsedEpicManifest, String> {
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
    let prereq_ids = r.u32()?;
    for _ in 0..prereq_ids {
        r.string()?;
    }
    let prereq = EpicPrereq {
        name: r.string()?,
        path: r.string()?,
        args: r.string()?,
    };
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
    Ok((chunks, files, feature, prereq))
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

/// Un chunk Epic pèse environ 1 Mo et les CDN publics coupent parfois une connexion
/// (reset, DNS, proxy). Sans nouvelle tentative, une seule coupure fait échouer un
/// téléchargement de plusieurs dizaines de Go, donc on retente chaque chunk en
/// repassant sur la liste des CDN avec un délai croissant.
fn epic_chunk_retry_delay(attempt: u32) -> Duration {
    Duration::from_millis(500 * (1 << attempt.min(5)))
}

/// Dort `delay` sans bloquer un thread du runtime, tout en restant réactif à la pause.
async fn sleep_while_running_async(control: &Control, delay: Duration) -> bool {
    let deadline = Instant::now() + delay;
    loop {
        if !control.wait_until_resumed_async().await {
            return false;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return true;
        }
        tokio::time::sleep(remaining.min(Duration::from_millis(250))).await;
    }
}

/// Un worker Epic : prend les chunks de la file et les télécharge sans jamais bloquer le
/// runtime, pour que toutes les requêtes en vol partagent la même connexion HTTP/2.
#[allow(clippy::too_many_arguments)]
async fn epic_chunk_worker(
    index: usize,
    limit: Arc<AdaptiveLimit>,
    jobs: Arc<Mutex<VecDeque<EpicChunk>>>,
    client: reqwest::Client,
    cdns: Vec<String>,
    feature: u32,
    control: Arc<Control>,
    result_tx: tokio::sync::mpsc::Sender<EpicWorkerMessage>,
) {
    loop {
        let chunk = {
            let mut queue = jobs.lock().unwrap_or_else(PoisonError::into_inner);
            if queue.is_empty() {
                break;
            }
            // Au-delà de la limite courante, le worker attend sans prendre de chunk.
            if index >= limit.target() {
                None
            } else {
                queue.pop_front()
            }
        };
        let Some(chunk) = chunk else {
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };
        let path = epic_chunk_path(&chunk, feature);
        let mut last_error = None;
        let mut outcome = None;
        for attempt in 0..EPIC_CHUNK_ATTEMPTS {
            if attempt > 0 {
                let delay = epic_chunk_retry_delay(attempt);
                download_log(format!(
                    "Epic chunk {path} attempt {attempt}/{EPIC_CHUNK_ATTEMPTS} failed ({}); retrying in {} ms",
                    last_error.as_deref().unwrap_or("unknown error"),
                    delay.as_millis()
                ));
                if !sleep_while_running_async(&control, delay).await {
                    outcome = Some(Err("cancelled".to_owned()));
                    break;
                }
            }
            if !control.wait_until_resumed_async().await {
                outcome = Some(Err("cancelled".to_owned()));
                break;
            }
            for base in &cdns {
                if !control.wait_until_resumed_async().await {
                    outcome = Some(Err("cancelled".to_owned()));
                    break;
                }
                let url = format!("{}/{}", base.trim_end_matches('/'), path);
                match fetch_epic_chunk_async(&client, &url, &chunk, &result_tx, &limit).await {
                    Ok(decoded) => {
                        outcome = Some(Ok(decoded));
                        break;
                    }
                    Err(error) if error == "cancelled" => {
                        outcome = Some(Err(error));
                        break;
                    }
                    Err(error) => last_error = Some(error),
                }
            }
            if outcome.is_some() {
                break;
            }
        }
        let result = outcome.unwrap_or_else(|| {
            Err(format!(
                "Could not fetch Epic chunk {path} from available CDNs{}.",
                last_error
                    .map(|error| format!(" ({error})"))
                    .unwrap_or_default()
            ))
        });
        if result_tx
            .send(EpicWorkerMessage::Finished {
                guid: chunk.guid,
                size: chunk.size,
                result,
            })
            .await
            .is_err()
        {
            break;
        }
    }
}

/// Télécharge un chunk Epic depuis une URL CDN et le décompresse.
async fn fetch_epic_chunk_async(
    client: &reqwest::Client,
    url: &str,
    chunk: &EpicChunk,
    progress_tx: &tokio::sync::mpsc::Sender<EpicWorkerMessage>,
    limit: &AdaptiveLimit,
) -> Result<Vec<u8>, String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .map_err(describe_request_error)?;
    static LOGGED_VERSION: std::sync::Once = std::sync::Once::new();
    LOGGED_VERSION.call_once(|| {
        download_log(format!(
            "Epic CDN negotiated {:?}{}",
            response.version(),
            if response.version() == reqwest::Version::HTTP_2 {
                ""
            } else {
                " (no multiplexing: parallel connections are used instead)"
            }
        ));
    });
    let content_length = response.content_length();
    let mut raw = Vec::with_capacity(content_length.unwrap_or(0).min(64 * 1024 * 1024) as usize);
    let mut last_reported = 0u64;
    loop {
        match response.chunk().await {
            Ok(Some(bytes)) => {
                limit.add_bytes(bytes.len() as u64);
                raw.extend_from_slice(&bytes);
                if raw.len() as u64 - last_reported >= 256 * 1024 {
                    let downloaded = raw.len() as u64;
                    send_epic_progress(progress_tx, chunk, downloaded, content_length)?;
                    last_reported = downloaded;
                }
            }
            Ok(None) => break,
            Err(error) => {
                reset_epic_progress(progress_tx, chunk);
                return Err(format!(
                    "response body read failed: {}",
                    error.without_url()
                ));
            }
        }
    }
    let downloaded = raw.len() as u64;
    if downloaded > last_reported {
        let _ = send_epic_progress(progress_tx, chunk, downloaded, content_length);
    }
    if let Some(length) = content_length
        && downloaded != length
    {
        reset_epic_progress(progress_tx, chunk);
        return Err(format!(
            "incomplete response body ({downloaded}/{length} bytes)"
        ));
    }
    let decoded = tokio::task::spawn_blocking(move || epic_chunk_data(&raw))
        .await
        .map_err(|error| format!("chunk decoding task failed: {error}"))
        .and_then(|result| result);
    match decoded {
        Ok(decoded) => Ok(decoded),
        Err(error) => {
            reset_epic_progress(progress_tx, chunk);
            Err(error)
        }
    }
}

fn send_epic_progress(
    progress_tx: &tokio::sync::mpsc::Sender<EpicWorkerMessage>,
    chunk: &EpicChunk,
    downloaded: u64,
    content_length: Option<u64>,
) -> Result<(), String> {
    let logical = content_length
        .filter(|length| *length > 0)
        .map(|length| {
            chunk
                .size
                .saturating_mul(downloaded)
                .checked_div(length)
                .unwrap_or(0)
                .min(chunk.size)
        })
        .unwrap_or(0);
    // Progress is cosmetic: when the queue is full it is dropped instead of slowing the
    // transfer down. A closed queue means the whole download was abandoned.
    match progress_tx.try_send(EpicWorkerMessage::Progress {
        guid: chunk.guid,
        downloaded,
        logical,
    }) {
        Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => Ok(()),
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => Err("cancelled".to_owned()),
    }
}

fn reset_epic_progress(
    progress_tx: &tokio::sync::mpsc::Sender<EpicWorkerMessage>,
    chunk: &EpicChunk,
) {
    let _ = progress_tx.try_send(EpicWorkerMessage::Progress {
        guid: chunk.guid,
        downloaded: 0,
        logical: 0,
    });
}

/// `reqwest` n'affiche que « error sending request » : on ajoute la chaîne des causes
/// (DNS, TLS, connexion fermée…) pour que l'erreur finale soit exploitable.
fn describe_request_error(error: reqwest::Error) -> String {
    let error = error.without_url();
    let mut message = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        let text = cause.to_string();
        if !text.is_empty() && !message.contains(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }
        source = cause.source();
    }
    message
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
        // Les chunks Epic décompressent à 1 Mio : évite les réallocations successives.
        let mut out = Vec::with_capacity(1 << 20);
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
        if error == "paused" || control.get() == ControlState::Paused {
            download_log(format!("Xbox download paused for product_id={product_id}"));
        } else {
            download_log(format!(
                "Xbox download failed for product_id={product_id}: {error}"
            ));
        }
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
    control: &Arc<Control>,
    tx: &mpsc::Sender<DownloadEvent>,
) -> Result<bool, String> {
    // The whole byte-range transfer runs on the async client: every segment stays in flight on
    // the same connection (HTTP/2 multiplexing when the CDN negotiates it).
    let runtime = download_runtime()?;
    let ranged: Option<u64> = match runtime.block_on(async {
        let client = async_client(Duration::from_secs(180))?;
        // Confirm that this particular package endpoint honors a bounded byte range. Some Xbox
        // asset hosts return 206 for an open-ended range while ignoring its requested end.
        let (total, supported) = probe_xbox_ranges(&client, url, expected_size).await;
        if !supported || total == 0 {
            return Ok::<_, String>(None);
        }
        download_log(format!(
            "Xbox server confirmed byte ranges: total={total}, segment_size={XBOX_RANGE_BYTES}, workers=adaptive {XBOX_WORKERS} (max {ADAPTIVE_MAX_WORKERS})"
        ));
        download_xbox_segments_async(&client, url, total, partial_path, id, control, tx).await?;
        Ok(Some(total))
    }) {
        Ok(value) => value,
        Err(error) if error == "cancelled" => {
            remove_partial(partial_path)?;
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    let Some(total) = ranged else {
        download_log(format!(
            "Xbox server does not confirm bounded ranges; using the regular stream (expected_size={expected_size})"
        ));
        // A file pre-sized by an earlier ranged attempt would look complete to the sequential
        // transfer, which resumes from the file length: start it from a clean file.
        if xbox_ranges_path(partial_path).exists() {
            let _ = fs::remove_file(xbox_ranges_path(partial_path));
            OpenOptions::new()
                .write(true)
                .open(partial_path)
                .and_then(|file| file.set_len(0))
                .map_err(|error| format!("Could not reset the partial package: {error}"))?;
        }
        return transfer(url, true, destination, partial_path, id, control, tx);
    };

    // The segments were written straight to their final offsets in the partial file: there is
    // nothing to assemble, so no second copy of the package is ever needed on disk.
    if !control.wait_until_resumed() {
        remove_partial(partial_path)?;
        return Ok(false);
    }
    let length = fs::metadata(partial_path)
        .map(|metadata| metadata.len())
        .map_err(|error| format!("Could not inspect the Xbox package: {error}"))?;
    if length != total {
        return Err("The Xbox package has an unexpected size after download.".into());
    }
    match fs::remove_file(xbox_ranges_path(partial_path)) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Could not remove the range journal: {error}")),
    }
    publish_progress(tx, id, total, total, 0);
    finish_file(partial_path, destination)?;
    Ok(true)
}

/// Vérifie qu'un CDN Xbox honore une plage bornée avant de découper le paquet en segments.
async fn probe_xbox_ranges(client: &reqwest::Client, url: &str, expected_size: u64) -> (u64, bool) {
    match client
        .get(url)
        .header(USER_AGENT, XBOX_ASSET_USER_AGENT)
        .header(RANGE, "bytes=0-0")
        .send()
        .await
    {
        Ok(response) => {
            download_log(format!(
                "Xbox CDN negotiated {:?}{}",
                response.version(),
                if response.version() == reqwest::Version::HTTP_2 {
                    ""
                } else {
                    " (no multiplexing: parallel connections are used instead)"
                }
            ));
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
            (total, valid)
        }
        Err(error) => {
            download_log(format!(
                "Xbox byte-range probe failed; using the regular stream: {}",
                error.without_url()
            ));
            (expected_size, false)
        }
    }
}

/// Journal de progression : suite d'enregistrements de 16 octets little-endian
/// (offset de début du segment, octets déjà écrits dans ce segment), ajoutés au fil de l'eau
/// après chaque écriture sur disque. Le paquet est écrit directement à son emplacement final :
/// pas d'assemblage ni de double espace disque, et la reprise se fait à l'octet près.
fn xbox_ranges_path(partial_path: &Path) -> PathBuf {
    let mut name = partial_path.file_name().unwrap_or_default().to_os_string();
    name.push(".journal");
    partial_path.with_file_name(name)
}

/// Octets déjà écrits par segment (la plus grande valeur journalisée l'emporte).
fn read_xbox_progress(path: &Path, total: u64) -> std::collections::HashMap<u64, u64> {
    let mut progress = std::collections::HashMap::new();
    let Ok(bytes) = fs::read(path) else {
        return progress;
    };
    for record in bytes.chunks_exact(16) {
        let mut start = [0u8; 8];
        let mut done = [0u8; 8];
        start.copy_from_slice(&record[..8]);
        done.copy_from_slice(&record[8..]);
        let start = u64::from_le_bytes(start);
        let done = u64::from_le_bytes(done).min(XBOX_RANGE_BYTES);
        if start % XBOX_RANGE_BYTES == 0 && start < total {
            let entry = progress.entry(start).or_insert(0);
            *entry = (*entry).max(done);
        }
    }
    progress
}

fn record_xbox_progress(path: &Path, start: u64, done: u64) -> io::Result<()> {
    let mut record = [0u8; 16];
    record[..8].copy_from_slice(&start.to_le_bytes());
    record[8..].copy_from_slice(&done.to_le_bytes());
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(&record)
}

/// Écriture positionnelle : plusieurs segments écrivent en parallèle dans le même fichier.
fn write_all_at(file: &File, mut buffer: &[u8], mut offset: u64) -> io::Result<()> {
    while !buffer.is_empty() {
        #[cfg(windows)]
        let written = {
            use std::os::windows::fs::FileExt;
            file.seek_write(buffer, offset)?
        };
        #[cfg(unix)]
        let written = {
            use std::os::unix::fs::FileExt;
            file.write_at(buffer, offset)?
        };
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        buffer = &buffer[written..];
        offset += written as u64;
    }
    Ok(())
}

/// Où journaliser la progression d'un segment une fois un bloc écrit.
struct SegmentJournal<'a> {
    path: &'a Path,
    segment_start: u64,
}

/// Écrit un bloc sur un thread dédié aux E/S bloquantes, pour ne pas figer les flux réseau,
/// puis journalise l'avancement du segment (les données d'abord, le journal ensuite).
async fn write_xbox_block(
    file: &Arc<File>,
    data: Vec<u8>,
    position: u64,
    journal: &SegmentJournal<'_>,
) -> Result<(), String> {
    let file = Arc::clone(file);
    let journal_path = journal.path.to_path_buf();
    let segment_start = journal.segment_start;
    tokio::task::spawn_blocking(move || {
        write_all_at(&file, &data, position)?;
        let done = position + data.len() as u64 - segment_start;
        record_xbox_progress(&journal_path, segment_start, done)
    })
    .await
    .map_err(|error| format!("Could not write Xbox data: {error}"))?
    .map_err(|error| format!("Could not write Xbox range segment: {error}"))
}

/// Télécharge les segments Xbox manquants directement à leur offset final dans le fichier
/// partiel. Le nombre de requêtes simultanées s'ajuste en direct selon le débit.
async fn download_xbox_segments_async(
    client: &reqwest::Client,
    url: &str,
    total: u64,
    partial_path: &Path,
    id: u64,
    control: &Arc<Control>,
    tx: &mpsc::Sender<DownloadEvent>,
) -> Result<(), String> {
    // Anciennes versions : segments `.range.N` séparés. Ils ne sont plus utilisés.
    if has_legacy_xbox_segments(partial_path) {
        remove_xbox_segments(partial_path, total)?;
    }
    let ranges_path = xbox_ranges_path(partial_path);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(partial_path)
        .map_err(|error| format!("Could not open the Xbox package file: {error}"))?;
    let current_len = file
        .metadata()
        .map(|metadata| metadata.len())
        .map_err(|error| format!("Could not inspect the Xbox package file: {error}"))?;
    let progress = if current_len == total {
        read_xbox_progress(&ranges_path, total)
    } else {
        // Nouveau téléchargement (ou fichier sans rapport) : on repart d'un fichier vide
        // de la taille finale. Un fichier creux (« sparse ») ne réserve pas l'espace et évite
        // le remplissage de zéros par NTFS lors des écritures hors séquence.
        let _ = fs::remove_file(&ranges_path);
        file.set_len(0)
            .map_err(|error| format!("Could not reset the Xbox package file: {error}"))?;
        #[cfg(windows)]
        if let Err(error) = mark_file_sparse(&file) {
            download_log(format!(
                "Could not mark the Xbox package as sparse ({error}); writes may be slow"
            ));
        }
        file.set_len(total)
            .map_err(|error| format!("Could not size the Xbox package file: {error}"))?;
        std::collections::HashMap::new()
    };
    let file = Arc::new(file);

    // (début, fin, octets déjà écrits) : un segment partiel reprend exactement où il s'est arrêté.
    let mut jobs = VecDeque::new();
    let mut already_received = 0u64;
    let mut segment_start = 0u64;
    while segment_start < total {
        let segment_end = segment_start
            .saturating_add(XBOX_RANGE_BYTES - 1)
            .min(total - 1);
        let length = segment_end - segment_start + 1;
        let done = progress
            .get(&segment_start)
            .copied()
            .unwrap_or(0)
            .min(length);
        already_received = already_received.saturating_add(done);
        if done < length {
            jobs.push_back((segment_start, segment_end, done));
        }
        segment_start = segment_end + 1;
    }
    let job_count = jobs.len();
    if job_count == 0 {
        return Ok(());
    }
    let progress_received = Arc::new(AtomicU64::new(already_received));
    let speed_meter = Arc::new(Mutex::new(SpeedMeter::new()));
    let jobs = Arc::new(Mutex::new(jobs));
    let limit = AdaptiveLimit::new(XBOX_WORKERS);
    let (result_tx, mut result_rx) = tokio::sync::mpsc::unbounded_channel::<Result<(), String>>();
    let mut workers = tokio::task::JoinSet::new();
    for index in 0..ADAPTIVE_MAX_WORKERS.min(job_count) {
        let jobs = Arc::clone(&jobs);
        let client = client.clone();
        let control = Arc::clone(control);
        let tx = tx.clone();
        let progress_received = Arc::clone(&progress_received);
        let speed_meter = Arc::clone(&speed_meter);
        let url = url.to_owned();
        let file = Arc::clone(&file);
        let ranges_path = ranges_path.clone();
        let result_tx = result_tx.clone();
        let limit = Arc::clone(&limit);
        workers.spawn(async move {
            loop {
                if control.get() != ControlState::Running {
                    break;
                }
                let job = {
                    let mut queue = jobs.lock().unwrap_or_else(PoisonError::into_inner);
                    if queue.is_empty() {
                        break;
                    }
                    // Au-delà de la limite courante, le worker attend sans prendre de segment.
                    if index >= limit.target() {
                        None
                    } else {
                        queue.pop_front()
                    }
                };
                let Some((start, end, done)) = job else {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                };
                let result = download_xbox_range_async(
                    &client,
                    url.as_str(),
                    total,
                    (start, end, done),
                    &file,
                    &ranges_path,
                    id,
                    &control,
                    &tx,
                    &progress_received,
                    &speed_meter,
                    &limit,
                )
                .await;
                if result_tx.send(result).is_err() {
                    break;
                }
            }
        });
    }
    workers.spawn(adaptive_controller(Arc::clone(&limit)));
    drop(result_tx);
    let mut completed = 0;
    let mut first_error = None;
    while completed < job_count {
        match result_rx.recv().await {
            Some(Ok(())) => completed += 1,
            // Pause : chaque worker vide son tampon dans le fichier et le journal avant de
            // s'arrêter, on les laisse donc finir au lieu de les interrompre.
            Some(Err(error)) if error == "paused" => completed += 1,
            Some(Err(error)) => {
                first_error = Some(error);
                break;
            }
            None => break,
        }
    }
    // Stoppe les workers restants et attend leur fin : plus aucune écriture ne peut avoir
    // lieu pendant que l'appelant nettoie ou reprend.
    workers.abort_all();
    while workers.join_next().await.is_some() {}
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
    let sync_file = Arc::clone(&file);
    tokio::task::spawn_blocking(move || sync_file.sync_all())
        .await
        .map_err(|error| format!("Could not flush the Xbox package: {error}"))?
        .map_err(|error| format!("Could not flush the Xbox package: {error}"))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn download_xbox_range_async(
    client: &reqwest::Client,
    url: &str,
    total: u64,
    (start, end, initial): (u64, u64, u64),
    file: &Arc<File>,
    ranges_path: &Path,
    id: u64,
    control: &Control,
    tx: &mpsc::Sender<DownloadEvent>,
    progress_received: &AtomicU64,
    speed_meter: &Mutex<SpeedMeter>,
    limit: &AdaptiveLimit,
) -> Result<(), String> {
    const FLUSH_BYTES: usize = 1024 * 1024;
    let expected = end - start + 1;
    let journal = SegmentJournal {
        path: ranges_path,
        segment_start: start,
    };
    // Reprise exacte : on repart de l'octet suivant le dernier octet journalisé, et les
    // nouvelles tentatives reprennent là où le flux s'est arrêté.
    let mut offset = initial;
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
            .await
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
        let mut buffer: Vec<u8> = Vec::with_capacity(FLUSH_BYTES);
        let mut buffer_position = request_start;
        let mut last_report = Instant::now();
        let mut read_error = None;
        loop {
            if control.get() != ControlState::Running {
                let paused = control.get() == ControlState::Paused;
                if paused && !buffer.is_empty() {
                    // Rien n'est perdu à la pause : le tampon part dans le fichier et le journal.
                    let started = Instant::now();
                    write_xbox_block(file, buffer, buffer_position, &journal).await?;
                    limit.add_write_time(started.elapsed());
                }
                return Err(if paused {
                    "paused".into()
                } else {
                    "cancelled".into()
                });
            }
            match response.chunk().await {
                Ok(Some(bytes)) => {
                    let count = bytes.len();
                    if offset.saturating_add(count as u64) > expected {
                        return Err("Xbox server sent more bytes than requested.".into());
                    }
                    buffer.extend_from_slice(&bytes);
                    offset += count as u64;
                    if buffer.len() >= FLUSH_BYTES {
                        let block = std::mem::replace(&mut buffer, Vec::with_capacity(FLUSH_BYTES));
                        let length = block.len() as u64;
                        let started = Instant::now();
                        write_xbox_block(file, block, buffer_position, &journal).await?;
                        limit.add_write_time(started.elapsed());
                        buffer_position += length;
                    }
                    limit.add_bytes(count as u64);
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
                Ok(None) => break,
                Err(error) => {
                    read_error = Some(error.without_url().to_string());
                    break;
                }
            }
        }
        if !buffer.is_empty() {
            let started = Instant::now();
                    write_xbox_block(file, buffer, buffer_position, &journal).await?;
                    limit.add_write_time(started.elapsed());
        }
        if offset == expected {
            // Le dernier enregistrement du journal indique segment complet (octets == taille).
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
        if !sleep_while_running_async(control, pause).await {
            return Err("cancelled".into());
        }
    }
    Ok(())
}

fn xbox_segment_path(partial_path: &Path, start: u64) -> PathBuf {
    let mut name = partial_path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".range.{start}"));
    partial_path.with_file_name(name)
}

/// Vrai si un journal de segments terminés existe : il y a de quoi reprendre.
fn has_xbox_range_data(partial_path: &Path) -> bool {
    fs::metadata(xbox_ranges_path(partial_path))
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false)
}

/// Octets déjà téléchargés d'un paquet Xbox partiel. Le fichier étant pré-dimensionné, sa
/// longueur ne reflète plus l'avancement : on compte les segments du journal.
fn resumable_xbox_bytes(partial_path: &Path) -> u64 {
    let length = fs::metadata(partial_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let journal = xbox_ranges_path(partial_path);
    if journal.exists() {
        read_xbox_progress(&journal, u64::MAX)
            .values()
            .sum::<u64>()
            .min(length)
    } else {
        length
    }
}

/// Anciennes versions : segments `.range.N` séparés.
fn has_legacy_xbox_segments(partial_path: &Path) -> bool {
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
        .any(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
}

fn remove_xbox_segments(partial_path: &Path, total: u64) -> Result<(), String> {
    let mut start = 0u64;
    while start < total {
        let path = xbox_segment_path(partial_path, start);
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Could not remove Xbox range segment: {error}")),
        }
        start = start.saturating_add(XBOX_RANGE_BYTES);
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
        .http2_adaptive_window(true)
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
    // Journal des segments Xbox éventuel (voir `xbox_ranges_path`).
    let _ = fs::remove_file(xbox_ranges_path(path));
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

    /// Les chunks Epic sont écrits dans un ordre arbitraire : un fichier ordinaire
    /// forcerait NTFS à remplir de zéros tout l'espace entre la fin du fichier et l'offset
    /// d'écriture. Ce test vérifie qu'un fichier marqué creux n'alloue que les octets
    /// réellement écrits.
    #[cfg(windows)]
    #[test]
    fn sparse_output_files_avoid_zero_filling_gaps() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let folder = std::env::temp_dir().join(format!("universal-downloader-sparse-{nonce}"));
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join("sparse.bin");
        let file = File::create(&path).unwrap();
        if mark_file_sparse(&file).is_err() {
            // The volume does not support sparse files; nothing to assert.
            let _ = fs::remove_dir_all(&folder);
            return;
        }
        drop(file);
        let chunk = vec![0xABu8; 1024 * 1024];
        let offset = 512 * 1024 * 1024u64;
        let mut output = OpenOptions::new().write(true).open(&path).unwrap();
        let started = Instant::now();
        output.seek(SeekFrom::Start(offset)).unwrap();
        output.write_all(&chunk).unwrap();
        output.flush().unwrap();
        drop(output);
        let logical = fs::metadata(&path).unwrap().len();
        let allocated = allocated_size(&path).unwrap();
        let elapsed = started.elapsed();
        let _ = fs::remove_dir_all(&folder);
        assert_eq!(logical, offset + chunk.len() as u64);
        assert!(
            allocated < 64 * 1024 * 1024,
            "sparse file allocated {allocated} bytes for one 1 MiB chunk"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "writing one sparse chunk took {elapsed:?}"
        );
    }

    #[cfg(windows)]
    fn allocated_size(path: &Path) -> io::Result<u64> {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetCompressedFileSizeW(file_name: *const u16, high: *mut u32) -> u32;
        }

        let wide: Vec<u16> = OsStr::new(path).encode_wide().chain(Some(0)).collect();
        let mut high = 0u32;
        let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
        if low == u32::MAX {
            let error = io::Error::last_os_error();
            if error.raw_os_error().unwrap_or(0) != 0 {
                return Err(error);
            }
        }
        Ok(((u64::from(high)) << 32) | u64::from(low))
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

    /// Les chunks Epic passent par un seul client asynchrone : toutes les requêtes restent en
    /// vol en même temps. Douze chunks servis avec 300 ms de latence prennent ~0,4 s en
    /// parallèle, contre 3,6 s si les requêtes étaient menées l'une après l'autre.
    #[test]
    fn epic_chunks_are_fetched_concurrently() {
        const CHUNKS: usize = 12;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let body = epic_chunk_fixture(1024);
        let server = thread::spawn(move || {
            for _ in 0..CHUNKS {
                let (mut stream, _) = listener.accept().unwrap();
                let body = body.clone();
                thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    let _ = reader.read_line(&mut line);
                    drop(reader);
                    thread::sleep(Duration::from_millis(300));
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .unwrap();
                    stream.write_all(&body).unwrap();
                });
            }
        });

        let runtime = download_runtime().unwrap();
        let started = Instant::now();
        let results = runtime.block_on(async {
            let client = async_client(Duration::from_secs(30)).unwrap();
            let (progress_tx, _progress_rx) = tokio::sync::mpsc::channel(1024);
            let limit = AdaptiveLimit::new(16);
            let mut tasks = Vec::new();
            for _ in 0..CHUNKS {
                let client = client.clone();
                let limit = Arc::clone(&limit);
                let progress_tx = progress_tx.clone();
                let url = format!("http://{address}/chunk");
                tasks.push(tokio::spawn(async move {
                    let chunk = EpicChunk {
                        guid: [0; 4],
                        hash: 0,
                        group: 0,
                        size: 1024,
                    };
                    fetch_epic_chunk_async(&client, &url, &chunk, &progress_tx, &limit).await
                }));
            }
            let mut results = Vec::new();
            for task in tasks {
                results.push(task.await.unwrap());
            }
            results
        });
        let elapsed = started.elapsed();
        server.join().unwrap();
        assert!(
            results
                .iter()
                .all(|result| result.as_ref().is_ok_and(|data| data.len() == 1024)),
            "unexpected chunk results: {results:?}"
        );
        assert!(
            elapsed < Duration::from_millis(1500),
            "{CHUNKS} delayed chunks took {elapsed:?}; they were not fetched concurrently"
        );
    }

    /// Les segments Xbox sont écrits directement à leur offset final : à la fin il ne reste ni
    /// segment `.range.N` ni journal, et le fichier final est identique à la source.
    #[test]
    fn xbox_ranges_are_written_in_place() {
        let total = XBOX_RANGE_BYTES * 2 + 12_345;
        let data: Arc<Vec<u8>> = Arc::new((0..total).map(|i| (i % 251) as u8).collect());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let served = Arc::clone(&data);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let data = Arc::clone(&served);
                thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut range = (0u64, 0u64);
                    let mut line = String::new();
                    loop {
                        line.clear();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                            break;
                        }
                        if let Some(value) = line.to_ascii_lowercase().strip_prefix("range: bytes=")
                        {
                            let (a, b) = value.trim().split_once('-').unwrap();
                            range = (a.parse().unwrap(), b.parse().unwrap());
                        }
                    }
                    let (a, b) = range;
                    let body = &data[a as usize..=b as usize];
                    write!(
                        stream,
                        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {a}-{b}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        data.len(),
                        body.len()
                    )
                    .unwrap();
                    let _ = stream.write_all(body);
                });
            }
        });

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let folder = std::env::temp_dir().join(format!("universal-downloader-xbox-{nonce}"));
        fs::create_dir_all(&folder).unwrap();
        let destination = folder.join("package.msixvc");
        let partial = partial_path(&destination);
        File::create(&partial).unwrap();
        let (tx, _rx) = mpsc::channel();
        let control = Arc::new(Control::new());
        let url = format!("http://{address}/package");
        let done = transfer_xbox_ranges(&url, total, &destination, &partial, 1, &control, &tx);
        assert!(done.unwrap());
        assert_eq!(fs::read(&destination).unwrap(), *data);
        let leftovers: Vec<_> = fs::read_dir(&folder)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        let _ = fs::remove_dir_all(&folder);
        assert_eq!(leftovers, vec!["package.msixvc".to_owned()]);
    }

    #[test]
    fn adaptive_concurrency_follows_throughput() {
        let mut tuner = AdaptiveTuner::new();
        let mut target = 16usize;
        // Stabilisation puis mesure de référence, puis premier palier d'essai.
        let mut rate = 10e6;
        for _ in 0..5 {
            target = tuner.next(target, rate);
        }
        assert_eq!(target, 16 + ADAPTIVE_STEP);
        // Le débit suit le nombre de connexions : on continue de monter.
        for _ in 0..20 {
            rate = target as f64 * 1e6;
            target = tuner.next(target, rate);
        }
        assert!(target > 16 + ADAPTIVE_STEP);
        // Plateau : on revient au palier précédent et on ne descend jamais plus bas.
        let plateau = target as f64 * 1e6;
        let mut lowest = target;
        for _ in 0..40 {
            target = tuner.next(target, plateau);
            lowest = lowest.min(target);
        }
        assert!(lowest + ADAPTIVE_STEP >= target);
        // Pause : rien ne bouge.
        assert_eq!(tuner.next(target, 0.0), target);
        // Les bornes sont respectées.
        let mut tuner = AdaptiveTuner::new();
        let mut target = ADAPTIVE_MAX_WORKERS;
        for _ in 0..30 {
            target = tuner.next(target, 1e8);
            assert!((ADAPTIVE_MIN_WORKERS..=ADAPTIVE_MAX_WORKERS).contains(&target));
        }
    }

    /// Une pause en plein segment ne perd aucun octet : la reprise repart exactement du dernier
    /// octet écrit.
    #[test]
    fn xbox_journal_resumes_at_the_byte() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let folder = std::env::temp_dir().join(format!("universal-downloader-journal-{nonce}"));
        fs::create_dir_all(&folder).unwrap();
        let partial = folder.join("package.msixvc.part");
        let journal = xbox_ranges_path(&partial);
        record_xbox_progress(&journal, 0, 1_048_576).unwrap();
        record_xbox_progress(&journal, 0, 2_097_152).unwrap();
        record_xbox_progress(&journal, XBOX_RANGE_BYTES, 777).unwrap();
        let progress = read_xbox_progress(&journal, XBOX_RANGE_BYTES * 3);
        let _ = fs::remove_dir_all(&folder);
        assert_eq!(progress.get(&0), Some(&2_097_152));
        assert_eq!(progress.get(&XBOX_RANGE_BYTES), Some(&777));
    }

    /// Construit un chunk Epic non compressé valide (en-tête version 1).
    fn epic_chunk_fixture(size: usize) -> Vec<u8> {
        const HEADER: u32 = 4 * 4 + 16 + 8 + 1;
        let mut raw = Vec::with_capacity(HEADER as usize + size);
        raw.extend_from_slice(&0xB1FE3AA2u32.to_le_bytes());
        raw.extend_from_slice(&1u32.to_le_bytes());
        raw.extend_from_slice(&HEADER.to_le_bytes());
        raw.extend_from_slice(&(size as u32).to_le_bytes());
        raw.extend_from_slice(&[0u8; 24]);
        raw.push(0);
        raw.extend_from_slice(&vec![0x5Au8; size]);
        raw
    }
}