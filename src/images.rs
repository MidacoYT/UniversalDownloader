//! Cache de textures chargées en arrière-plan + helpers de rendu des jaquettes.
//! Les miniatures sont aussi mises en cache sur disque pour ne pas retélécharger
//! toutes les jaquettes à chaque lancement.
use crate::net;
use eframe::egui::{self, Color32, CornerRadius, Rect, Vec2};
use sha1::{Digest, Sha1};
use std::{
    cell::Cell,
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

type Loaded = (String, Option<egui::ColorImage>);
type Job = (String, String, (u32, u32), egui::Context);

/// Nombre de téléchargements d'images simultanés (évite un thread par jaquette).
const WORKERS: usize = 6;

/// Nouvelle tentative pour une jaquette en échec : 5 essais, une minute entre chacun.
const MAX_ATTEMPTS: u32 = 5;
const RETRY_DELAY: Duration = Duration::from_secs(60);

/// Budget mémoire des jaquettes chargées (RGBA 8 bits). Au-delà, les textures inutilisées
/// depuis un moment sont libérées : elles se rechargent depuis le cache disque si on y revient.
/// Sans limite, parcourir tout le catalogue accumulait des milliers de textures.
const TEXTURE_BUDGET: usize = 256 * 1024 * 1024;

/// Une texture demandée il y a moins de ce nombre de frames n'est jamais évincée
/// (évite de décharger ce qui est à l'écran et le clignotement qui s'ensuivrait).
const KEEP_FRAMES: u64 = 120;

/// Texture chargée + dernière frame où elle a été demandée (éviction LRU).
struct CachedTexture {
    texture: egui::TextureHandle,
    last_used: Cell<u64>,
    bytes: usize,
}

pub fn hero_key(game_id: &str) -> String {
    format!("hero:{game_id}")
}

/// Une jaquette réseau/lecture en attente de résultat.
#[derive(Clone)]
struct Pending {
    url: String,
    max: (u32, u32),
    attempts: u32,
}

/// Une jaquette échouée, re-tentée automatiquement à `at`.
#[derive(Clone)]
struct Failed {
    url: String,
    max: (u32, u32),
    attempts: u32,
    at: Instant,
}

pub struct ImageCache {
    rx: Receiver<Loaded>,
    jobs: Sender<Job>,
    pending: HashMap<String, Pending>,
    failed: HashMap<String, Failed>,
    textures: HashMap<String, CachedTexture>,
    texture_bytes: usize,
    /// Compteur de frames (incrémenté dans `poll`) servant d'horloge à l'éviction.
    frame: u64,
}

impl ImageCache {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<Loaded>();
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        for _ in 0..WORKERS {
            let job_rx = Arc::clone(&job_rx);
            let tx = tx.clone();
            std::thread::spawn(move || {
                // Le thread s'arrête quand le cache (donc `jobs`) est détruit.
                // Le verrou n'est tenu que le temps de recevoir la tâche (pas pendant le
                // téléchargement), sinon les workers se bloqueraient les uns les autres.
                loop {
                    let job = job_rx.lock().unwrap_or_else(PoisonError::into_inner).recv();
                    let Ok((key, url, max, ctx)) = job else { break };
                    let _ = tx.send((key, load_image(&url, max)));
                    ctx.request_repaint();
                }
            });
        }
        Self {
            rx,
            jobs,
            pending: HashMap::new(),
            failed: HashMap::new(),
            textures: HashMap::new(),
            texture_bytes: 0,
            frame: 0,
        }
    }

    pub fn get(&self, key: &str) -> Option<&egui::TextureHandle> {
        self.textures.get(key).map(|cached| {
            cached.last_used.set(self.frame);
            &cached.texture
        })
    }

    /// Autorise une nouvelle tentative immédiate pour les images qui avaient échoué.
    pub fn clear_failed(&mut self) {
        self.failed.clear();
    }

    /// Lance le téléchargement si l'image n'est ni chargée, ni en cours, ni en échec.
    pub fn request(&mut self, ctx: &egui::Context, key: &str, url: &str, max: (u32, u32)) {
        self.schedule(ctx, key, url, max, 0);
    }

    fn schedule(
        &mut self,
        ctx: &egui::Context,
        key: &str,
        url: &str,
        max: (u32, u32),
        attempts: u32,
    ) {
        if url.is_empty()
            || self.textures.contains_key(key)
            || self.failed.contains_key(key)
            || self.pending.contains_key(key)
        {
            return;
        }
        let job = (key.to_owned(), url.to_owned(), max, ctx.clone());
        if self.jobs.send(job).is_ok() {
            self.pending.insert(
                key.to_owned(),
                Pending {
                    url: url.to_owned(),
                    max,
                    attempts,
                },
            );
        }
    }

    /// Intègre les images terminées, puis relance les échecs arrivés à échéance
    /// (à appeler une fois par frame).
    pub fn poll(&mut self, ctx: &egui::Context) {
        self.frame = self.frame.wrapping_add(1);
        let mut changed = false;
        while let Ok((key, image)) = self.rx.try_recv() {
            let Some(pending) = self.pending.remove(&key) else {
                continue;
            };
            match image {
                Some(image) => {
                    let texture =
                        ctx.load_texture(format!("img-{key}"), image, egui::TextureOptions::LINEAR);
                    let [width, height] = texture.size();
                    let bytes = width * height * 4;
                    self.texture_bytes += bytes;
                    let cached = CachedTexture {
                        texture,
                        last_used: Cell::new(self.frame),
                        bytes,
                    };
                    if let Some(old) = self.textures.insert(key, cached) {
                        self.texture_bytes = self.texture_bytes.saturating_sub(old.bytes);
                    }
                    changed = true;
                }
                None => {
                    let attempts = pending.attempts + 1;
                    if attempts < MAX_ATTEMPTS {
                        self.failed.insert(
                            key,
                            Failed {
                                url: pending.url,
                                max: pending.max,
                                attempts,
                                at: Instant::now() + RETRY_DELAY,
                            },
                        );
                        changed = false;
                    }
                }
            }
        }
        // Relance automatique des échecs dont le délai est écoulé.
        let now = Instant::now();
        let due: Vec<(String, Failed)> = self
            .failed
            .iter()
            .filter(|(_, failed)| failed.at <= now)
            .map(|(key, failed)| (key.clone(), failed.clone()))
            .collect();
        for (key, failed) in due {
            self.failed.remove(&key);
            self.schedule(ctx, &key, &failed.url, failed.max, failed.attempts);
        }
        if self.texture_bytes > TEXTURE_BUDGET {
            self.evict_idle();
        }
        if changed {
            ctx.request_repaint();
        }
    }

    /// Libère les textures les moins récemment utilisées jusqu'à repasser à 75 % du budget.
    fn evict_idle(&mut self) {
        let frame = self.frame;
        let mut idle: Vec<(u64, String)> = self
            .textures
            .iter()
            .filter(|(_, cached)| cached.last_used.get().saturating_add(KEEP_FRAMES) < frame)
            .map(|(key, cached)| (cached.last_used.get(), key.clone()))
            .collect();
        idle.sort_unstable();
        let target = TEXTURE_BUDGET / 4 * 3;
        for (_, key) in idle {
            if self.texture_bytes <= target {
                break;
            }
            if let Some(cached) = self.textures.remove(&key) {
                self.texture_bytes = self.texture_bytes.saturating_sub(cached.bytes);
            }
        }
    }
}

/// Dossier du cache disque des jaquettes.
fn covers_dir() -> Option<PathBuf> {
    let root = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(root.join("UniversalDownloader").join("covers"))
}

/// Emplacement du fichier miniature en cache pour cette URL et taille cible.
fn cached_path(url: &str, max: (u32, u32)) -> Option<PathBuf> {
    let digest = Sha1::digest(url.as_bytes());
    Some(covers_dir()?.join(format!("{digest:x}-{}x{}.jpg", max.0, max.1)))
}

fn to_color_image(image: &image::DynamicImage) -> egui::ColorImage {
    let rgba = image.to_rgba8();
    egui::ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    )
}

fn load_image(url: &str, max: (u32, u32)) -> Option<egui::ColorImage> {
    // 1. Cache disque : de toute façon à la taille demandée (clé = URL + dimensions).
    if let Some(path) = cached_path(url, max)
        && let Ok(bytes) = fs::read(&path)
        && let Ok(image) = image::load_from_memory(&bytes)
    {
        return Some(to_color_image(&image));
    }

    // 2. Réseau + miniature, puis écriture du cache (sans bloquer si le disque refuse).
    let bytes = net::shared_client()?
        .get(url)
        .send()
        .ok()?
        .error_for_status()
        .ok()?
        .bytes()
        .ok()?;
    // Les jaquettes sont opaques : on retire le canal alpha, sinon l'encodeur JPEG refuse
    // les sources PNG/WebP RGBA et le cache disque n'était jamais écrit pour elles.
    let thumbnail = image::DynamicImage::ImageRgb8(
        image::load_from_memory(&bytes)
            .ok()?
            .thumbnail(max.0, max.1)
            .to_rgb8(),
    );
    if let Some(path) = cached_path(url, max) {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = thumbnail.save(&path);
    }
    Some(to_color_image(&thumbnail))
}

// ───────────────────────── Rendu ─────────────────────────

pub fn top_radius(radius: u8) -> CornerRadius {
    CornerRadius {
        nw: radius,
        ne: radius,
        sw: 0,
        se: 0,
    }
}

/// UV « cover » : remplit la cible en rognant au centre. `zoom` ≥ 1.0 agrandit.
pub fn cover_uv(image: Vec2, target: Vec2, zoom: f32) -> Rect {
    let image_aspect = image.x / image.y;
    let target_aspect = target.x / target.y;
    let (w, h) = if image_aspect > target_aspect {
        (target_aspect / image_aspect, 1.0)
    } else {
        (1.0, image_aspect / target_aspect)
    };
    Rect::from_center_size(egui::pos2(0.5, 0.5), egui::vec2(w / zoom, h / zoom))
}

/// Peint la texture en mode « cover » dans `rect` sans toucher au curseur de layout.
pub fn paint_cover(
    ui: &egui::Ui,
    texture: &egui::TextureHandle,
    rect: Rect,
    radius: CornerRadius,
    zoom: f32,
) {
    egui::Image::from_texture(texture)
        .uv(cover_uv(texture.size_vec2(), rect.size(), zoom))
        .corner_radius(radius)
        .paint_at(ui, rect);
}

pub fn paint_placeholder(ui: &egui::Ui, rect: Rect, radius: CornerRadius, text: &str) {
    ui.painter()
        .rect_filled(rect, radius, Color32::from_rgb(25, 25, 40));
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(12.0),
        Color32::from_rgb(148, 148, 168),
    );
}
