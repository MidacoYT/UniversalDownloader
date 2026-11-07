//! Cache de textures chargées en arrière-plan + helpers de rendu des jaquettes.
//! Les miniatures sont aussi mises en cache sur disque pour ne pas retélécharger
//! toutes les jaquettes à chaque lancement.
use crate::net;
use eframe::egui::{self, Color32, CornerRadius, Rect, Vec2};
use sha1::{Digest, Sha1};
use std::{
    fs,
    path::PathBuf,
    collections::HashMap,
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
    textures: HashMap<String, egui::TextureHandle>,
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
        }
    }

    pub fn get(&self, key: &str) -> Option<&egui::TextureHandle> {
        self.textures.get(key)
    }

    /// Autorise une nouvelle tentative immédiate pour les images qui avaient échoué.
    pub fn clear_failed(&mut self) {
        self.failed.clear();
    }

    /// Lance le téléchargement si l'image n'est ni chargée, ni en cours, ni en échec.
    pub fn request(&mut self, ctx: &egui::Context, key: &str, url: &str, max: (u32, u32)) {
        self.schedule(ctx, key, url, max, 0);
    }

    fn schedule(&mut self, ctx: &egui::Context, key: &str, url: &str, max: (u32, u32), attempts: u32) {
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
        let mut changed = false;
        while let Ok((key, image)) = self.rx.try_recv() {
            let Some(pending) = self.pending.remove(&key) else {
                continue;
            };
            match image {
                Some(image) => {
                    let texture = ctx.load_texture(
                        format!("img-{key}"),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.textures.insert(key, texture);
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
        if changed {
            ctx.request_repaint();
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
    Some(covers_dir()?.join(format!(
        "{digest:x}-{}x{}.jpg",
        max.0, max.1
    )))
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
    let thumbnail = image::load_from_memory(&bytes).ok()?.thumbnail(max.0, max.1);
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
