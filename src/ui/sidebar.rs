//! Barre latérale : logo, navigation, carte du téléchargement en cours (clic → page Downloads).
//! Tout est aligné sur une même marge gauche (INSET) : logo, titre de section, icônes, pied de page.
use super::{Action, Page, icons, theme, widgets};
use crate::catalog::Catalog;
use crate::downloads::{Download, DownloadManager, DownloadStatus};
use crate::images::{self, ImageCache};
use eframe::egui::{self, Color32, RichText};

const INSET: f32 = 12.0;
const ICON_SIZE: f32 = 17.0;
const ICON_GAP: f32 = 12.0;
const ITEM_HEIGHT: f32 = 36.0;
const DOWNLOAD_CARD_HEIGHT: f32 = 60.0;

/// Résumé du téléchargement affiché en bas de la sidebar.
pub struct DownloadSummary {
    pub label: &'static str,
    pub name: String,
    pub speed: u64,
    pub eta_secs: Option<u64>,
    pub progress: Option<f32>,
    pub paused: bool,
    /// Nombre d'autres téléchargements actifs (en file d'attente ou en pause).
    pub more: usize,
    /// (id, url) de la jaquette du jeu, si on la connaît.
    pub cover: Option<(String, String)>,
}

impl DownloadSummary {
    /// Téléchargement « courant » : celui qui tourne, sinon le premier de la file (ordre de lancement).
    pub fn current(
        downloads: &DownloadManager,
        catalog: &Catalog,
        epic_catalog: &Catalog,
    ) -> Option<Self> {
        let mut active: Vec<&Download> = downloads
            .entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry.status,
                    DownloadStatus::Queued
                        | DownloadStatus::Downloading
                        | DownloadStatus::Paused
                        | DownloadStatus::Extracting
                        | DownloadStatus::Registering
                )
            })
            .collect();
        active.sort_by_key(|entry| {
            (
                !matches!(
                    entry.status,
                    DownloadStatus::Downloading
                        | DownloadStatus::Extracting
                        | DownloadStatus::Registering
                ),
                entry.id,
            )
        });
        let entry = *active.first()?;
        let game = catalog
            .games
            .iter()
            .chain(epic_catalog.games.iter())
            .find(|game| game.name.eq_ignore_ascii_case(&entry.name));
        let label = match entry.status {
            DownloadStatus::Queued => "Queued",
            DownloadStatus::Paused => "Paused",
            DownloadStatus::Extracting => "Extracting",
            DownloadStatus::Registering => "Registering",
            _ => "Downloading",
        };
        let eta_secs = (entry.status == DownloadStatus::Downloading
            && entry.speed > 0
            && entry.total > entry.received)
            .then(|| (entry.total - entry.received).div_ceil(entry.speed));
        let progress =
            (entry.total > 0).then(|| (entry.received as f32 / entry.total as f32).clamp(0.0, 1.0));
        Some(Self {
            label,
            name: entry.name.clone(),
            speed: if entry.status == DownloadStatus::Downloading {
                entry.speed
            } else {
                0
            },
            eta_secs,
            progress,
            paused: entry.status == DownloadStatus::Paused,
            more: active.len() - 1,
            cover: game
                .filter(|game| !game.image.is_empty())
                .map(|game| (game.id.clone(), game.image.clone())),
        })
    }
}

#[allow(dead_code)]
pub fn show(
    ui: &mut egui::Ui,
    current: Page,
    download: Option<&DownloadSummary>,
    images: &mut ImageCache,
    out: &mut Vec<Action>,
) {
    ui.spacing_mut().item_spacing.y = 2.0;
    ui.add_space(22.0);
    brand(ui);
    ui.add_space(26.0);
    section_label(ui, "MENU");
    ui.add_space(6.0);

    let items = [
        (Page::Home, "Home", icons::dashboard(ICON_SIZE)),
        (Page::Catalog, "Catalog", icons::catalog(ICON_SIZE)),
        (Page::Library, "Library", icons::folder(ICON_SIZE)),
        (Page::Downloads, "Downloads", icons::download(ICON_SIZE)),
        (Page::Updates, "Updates", icons::calendar(ICON_SIZE)),
        (Page::Settings, "Settings", icons::settings(ICON_SIZE)),
    ];
    for (page, label, image) in items {
        if nav_item(ui, label, image, current == page).clicked() {
            out.push(Action::Navigate(page));
        }
    }
    footer(ui, current, download, images, out);
}

fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(INSET + 4.0);
        ui.label(
            RichText::new(text)
                .size(9.5)
                .strong()
                .color(theme::TEXT_SUBTLE),
        );
    });
}

/// Logo + nom, sobre : une lettre dans un carré bleu, nom en capitales espacées.
fn brand(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.add_space(INSET);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::hover());
        ui.painter().rect_filled(rect, 6, theme::ACCENT);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "U",
            egui::FontId::proportional(16.0),
            Color32::WHITE,
        );
        ui.add_space(8.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(
                RichText::new("UNIVERSAL")
                    .size(13.0)
                    .strong()
                    .color(theme::TEXT_TITLE),
            );
            ui.label(
                RichText::new("DOWNLOADER")
                    .size(9.5)
                    .color(theme::TEXT_MUTED),
            );
        });
    });
}

/// Entrée de navigation : texte clair, indicateur bleu à gauche quand la page est active.
fn nav_item(
    ui: &mut egui::Ui,
    label: &str,
    image: egui::Image<'static>,
    selected: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ITEM_HEIGHT),
        egui::Sense::click(),
    );
    let selection = ui
        .ctx()
        .animate_bool_with_time(response.id.with("selected"), selected, 0.2);
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.12);

    let painter = ui.painter();
    if selection > 0.0 {
        painter.rect_filled(rect, 6, theme::accent_alpha((selection * 28.0) as u8));
    } else if hover > 0.0 {
        painter.rect_filled(rect, 6, Color32::from_white_alpha((hover * 10.0) as u8));
    }
    if selection > 0.0 {
        let bar = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 1.5, rect.center().y),
            egui::vec2(3.0, rect.height() * 0.55 * selection),
        );
        painter.rect_filled(bar, 2, theme::ACCENT);
    }

    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + INSET, rect.center().y - ICON_SIZE / 2.0),
        egui::vec2(ICON_SIZE, ICON_SIZE),
    );
    image
        .tint(if selected {
            theme::ACCENT_LIGHT
        } else {
            theme::TEXT_MUTED
        })
        .paint_at(ui, icon_rect);

    painter.text(
        egui::pos2(icon_rect.right() + ICON_GAP, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.0),
        if selected {
            theme::TEXT_TITLE
        } else {
            Color32::from_gray(190)
        },
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn footer(
    ui: &mut egui::Ui,
    current: Page,
    download: Option<&DownloadSummary>,
    images: &mut ImageCache,
    out: &mut Vec<Action>,
) {
    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(INSET as i8, 0))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add_space(14.0);
                ui.label(
                    RichText::new("Xbox · Epic Games")
                        .size(10.0)
                        .color(theme::TEXT_SUBTLE),
                );
            });
        if let Some(download) = download {
            ui.add_space(8.0);
            if download_card(ui, download, images, current == Page::Downloads).clicked() {
                out.push(Action::Navigate(Page::Downloads));
            }
        }
    });
}

/// Carte « téléchargement en cours » : fond sobre, jaquette, état, vitesse, barre fine.
fn download_card(
    ui: &mut egui::Ui,
    download: &DownloadSummary,
    images: &mut ImageCache,
    on_downloads_page: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), DOWNLOAD_CARD_HEIGHT),
        egui::Sense::click(),
    );
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.12);
    let accent = if download.paused {
        theme::GOLD
    } else {
        theme::ACCENT
    };

    let painter = ui.painter();
    painter.rect_filled(rect, 6, theme::mix((22, 26, 34), (30, 35, 46), hover));
    painter.rect_stroke(
        rect,
        6,
        egui::Stroke::new(
            1.0,
            if on_downloads_page {
                theme::ACCENT
            } else {
                theme::BORDER
            },
        ),
        egui::StrokeKind::Inside,
    );

    let cover_rect =
        egui::Rect::from_min_size(rect.min + egui::vec2(10.0, 9.0), egui::vec2(36.0, 36.0));
    let radius = egui::CornerRadius::same(4);
    let texture = match &download.cover {
        Some((id, url)) => {
            images.request(ui.ctx(), id, url, (120, 120));
            images.get(id)
        }
        None => None,
    };
    match texture {
        Some(texture) => images::paint_cover(ui, texture, cover_rect, radius, 1.0),
        None => {
            painter.rect_filled(cover_rect, radius, theme::BG_CARD_HOVER);
            painter.text(
                cover_rect.center(),
                egui::Align2::CENTER_CENTER,
                "↓",
                egui::FontId::proportional(16.0),
                theme::ACCENT_LIGHT,
            );
        }
    }

    let text_x = cover_rect.right() + 10.0;
    let chevron_center = egui::pos2(rect.right() - 16.0, cover_rect.center().y);
    let text_width = (chevron_center.x - 10.0 - text_x).max(20.0);
    painter.text(
        egui::pos2(text_x, cover_rect.top() + 8.0),
        egui::Align2::LEFT_CENTER,
        download.label,
        egui::FontId::proportional(12.5),
        theme::TEXT_TITLE,
    );
    let line_y = cover_rect.bottom() - 8.0;
    if download.speed > 0 {
        let muted = theme::TEXT_MUTED;
        let mut x = text_x;
        icons::download(11.0).tint(muted).paint_at(
            ui,
            egui::Rect::from_center_size(egui::pos2(x + 5.5, line_y), egui::vec2(11.0, 11.0)),
        );
        x += 15.0;
        let speed = painter.text(
            egui::pos2(x, line_y),
            egui::Align2::LEFT_CENTER,
            compact_speed(download.speed),
            egui::FontId::proportional(11.0),
            muted,
        );
        if let Some(seconds) = download.eta_secs {
            x = speed.right() + 12.0;
            paint_hourglass(painter, egui::pos2(x + 3.5, line_y), muted);
            painter.text(
                egui::pos2(x + 11.0, line_y),
                egui::Align2::LEFT_CENTER,
                compact_eta(seconds),
                egui::FontId::proportional(11.0),
                muted,
            );
        }
    } else {
        widgets::paint_truncated(
            painter,
            egui::pos2(text_x, line_y - 7.0),
            &download.name,
            egui::FontId::proportional(11.0),
            theme::TEXT_MUTED,
            text_width,
        );
    }

    icons::chevron_right(14.0).tint(theme::TEXT_MUTED).paint_at(
        ui,
        egui::Rect::from_center_size(chevron_center, egui::vec2(14.0, 14.0)),
    );

    let bar_y = rect.bottom() - 8.0;
    let bar_left = rect.left() + 12.0;
    let bar_right = rect.right() - 12.0;
    painter.line_segment(
        [egui::pos2(bar_left, bar_y), egui::pos2(bar_right, bar_y)],
        egui::Stroke::new(2.0, theme::BORDER),
    );
    let fraction = match download.progress {
        Some(progress) => progress,
        None => (ui.input(|i| i.time) as f32 * 0.5).fract(),
    };
    let dot_x = bar_left + (bar_right - bar_left) * fraction;
    if download.progress.is_some() && fraction > 0.0 {
        painter.line_segment(
            [egui::pos2(bar_left, bar_y), egui::pos2(dot_x, bar_y)],
            egui::Stroke::new(2.0, accent),
        );
    }
    painter.circle_filled(egui::pos2(dot_x, bar_y), 3.0, accent);

    let tooltip = if download.more > 0 {
        format!(
            "{}\n+{} queued — click to open downloads",
            download.name, download.more
        )
    } else {
        format!("{}\nClick to open downloads", download.name)
    };
    response
        .on_hover_text(tooltip)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Petit sablier dessiné à la main (aucune police ni icône SVG nécessaire).
fn paint_hourglass(painter: &egui::Painter, center: egui::Pos2, color: Color32) {
    let (half_w, half_h) = (3.5, 5.0);
    for sign in [-1.0, 1.0] {
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(center.x - half_w, center.y + sign * half_h),
                egui::pos2(center.x + half_w, center.y + sign * half_h),
                egui::pos2(center.x, center.y),
            ],
            color,
            egui::Stroke::NONE,
        ));
    }
}

/// « 11 MiB/s », « 2.4 MiB/s » : court pour tenir dans la carte.
fn compact_speed(bytes_per_second: u64) -> String {
    let units = ["B/s", "KiB/s", "MiB/s", "GiB/s"];
    let exponent = ((bytes_per_second.max(1) as f64).log(1024.0).floor() as usize).min(3);
    let value = bytes_per_second as f64 / 1024_f64.powi(exponent as i32);
    if value >= 10.0 {
        format!("{value:.0} {}", units[exponent])
    } else {
        format!("{value:.1} {}", units[exponent])
    }
}

/// « 42s », « 56m », « 2h 05m ».
fn compact_eta(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60)
    }
}
