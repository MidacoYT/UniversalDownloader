//! Cache de textures chargées en arrière-plan + helpers de rendu des jaquettes.
use crate::net;
use eframe::egui::{self, Color32, CornerRadius, Rect, Vec2};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};

type Loaded = (String, Option<egui::ColorImage>);
type Job = (String, String, (u32, u32), egui::Context);

/// Nombre de téléchargements d'images simultanés (évite un thread par jaquette).
const WORKERS: usize = 6;

pub fn hero_key(game_id: &str) -> String {
    format!("hero:{game_id}")
}

pub struct ImageCache {
    rx: Receiver<Loaded>,
    jobs: Sender<Job>,
    pending: HashSet<String>,
    failed: HashSet<String>,
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
            pending: HashSet::new(),
            failed: HashSet::new(),
            textures: HashMap::new(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&egui::TextureHandle> {
        self.textures.get(key)
    }

    /// Autorise une nouvelle tentative pour les images qui avaient échoué.
    pub fn clear_failed(&mut self) {
        self.failed.clear();
    }

    /// Lance le téléchargement si l'image n'est ni chargée, ni en cours, ni en échec.
    pub fn request(&mut self, ctx: &egui::Context, key: &str, url: &str, max: (u32, u32)) {
        if url.is_empty()
            || self.textures.contains_key(key)
            || self.failed.contains(key)
            || !self.pending.insert(key.to_owned())
        {
            return;
        }
        let job = (key.to_owned(), url.to_owned(), max, ctx.clone());
        if self.jobs.send(job).is_err() {
            self.pending.remove(key);
        }
    }

    /// Intègre les images terminées (à appeler une fois par frame).
    pub fn poll(&mut self, ctx: &egui::Context) {
        let mut changed = false;
        while let Ok((key, image)) = self.rx.try_recv() {
            self.pending.remove(&key);
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
                    self.failed.insert(key);
                }
            }
        }
        if changed {
            ctx.request_repaint();
        }
    }
}

fn load_image(url: &str, max: (u32, u32)) -> Option<egui::ColorImage> {
    let bytes = net::shared_client()?
        .get(url)
        .send()
        .ok()?
        .error_for_status()
        .ok()?
        .bytes()
        .ok()?;
    let image = image::load_from_memory(&bytes)
        .ok()?
        .thumbnail(max.0, max.1)
        .to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [image.width() as usize, image.height() as usize],
        image.as_raw(),
    ))
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
