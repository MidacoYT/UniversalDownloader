//! Téléchargements : en-tête avec débit global, filtres, transferts en cours avec
//! progression et commandes, historique des jeux terminés et liste des échecs.
use super::{icons, theme, widgets};
use crate::catalog::Catalog;
use crate::downloads::{Download, DownloadManager, DownloadStatus};
use crate::images::{self, ImageCache};
use crate::model::{Game, GameStore};
use crate::util::{format_size, open_folder};
use eframe::egui::{self, Color32, RichText};

const DONE_MIN_WIDTH: f32 = 360.0;
const GRID_GAP: f32 = 14.0;

/// Filtre d'affichage, conservé d'une frame à l'autre dans la mémoire de l'UI.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Filter {
    #[default]
    All,
    Active,
    Done,
    Failed,
}

impl Filter {
    const ALL: [Self; 4] = [Self::All, Self::Active, Self::Done, Self::Failed];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Active => "Active",
            Self::Done => "Completed",
            Self::Failed => "Failed",
        }
    }
}

#[derive(Clone, Copy)]
enum DownloadAction {
    Pause,
    Resume,
    Cancel,
}

pub fn show(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    epic_catalog: &Catalog,
    images: &mut ImageCache,
    downloads: &mut DownloadManager,
) {
    let filter_id = ui.id().with("downloads_filter");
    let mut filter = ui.data_mut(|d| d.get_temp::<Filter>(filter_id).unwrap_or_default());

    header(ui, &downloads.entries);
    ui.add_space(18.0);
    stats(ui, &downloads.entries);
    ui.add_space(20.0);
    filter_bar(ui, &mut filter);
    ui.data_mut(|d| d.insert_temp(filter_id, filter));
    ui.add_space(14.0);

    let counts = Counts::of(&downloads.entries);
    let visible = match filter {
        Filter::All => counts.active + counts.done + counts.failed,
        Filter::Active => counts.active,
        Filter::Done => counts.done,
        Filter::Failed => counts.failed,
    };

    let mut actions = Vec::new();
    egui::ScrollArea::vertical()
        .id_salt("downloads_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if !downloads.notice.is_empty() {
                ui.label(RichText::new(&downloads.notice).color(theme::DANGER));
                ui.add_space(10.0);
            }
            if visible == 0 {
                empty_state(ui, filter, downloads.entries.is_empty());
            }
            if matches!(filter, Filter::All | Filter::Active) {
                active_section(
                    ui,
                    catalog,
                    epic_catalog,
                    images,
                    &downloads.entries,
                    &mut actions,
                );
            }
            if matches!(filter, Filter::All | Filter::Done) {
                done_section(ui, catalog, epic_catalog, images, &downloads.entries);
            }
            if matches!(filter, Filter::All | Filter::Failed) {
                failed_section(ui, &downloads.entries);
            }
            ui.add_space(40.0);
        });

    for (id, action) in actions {
        match action {
            DownloadAction::Pause => downloads.pause(id),
            DownloadAction::Resume => downloads.resume(id),
            DownloadAction::Cancel => downloads.cancel(id),
        }
    }
}

// ───────────────────────── En-tête et statistiques ─────────────────────────

struct Counts {
    active: usize,
    done: usize,
    failed: usize,
    speed: u64,
}

impl Counts {
    fn of(entries: &[Download]) -> Self {
        Self {
            active: entries.iter().filter(|e| is_active(&e.status)).count(),
            done: entries
                .iter()
                .filter(|e| e.status == DownloadStatus::Completed)
                .count(),
            failed: entries
                .iter()
                .filter(|e| matches!(e.status, DownloadStatus::Failed(_)))
                .count(),
            speed: entries
                .iter()
                .filter(|e| is_running(&e.status))
                .map(|e| e.speed)
                .sum(),
        }
    }
}

fn header(ui: &mut egui::Ui, entries: &[Download]) {
    let counts = Counts::of(entries);
    ui.add_space(24.0);
    ui.vertical(|ui| {
        ui.label(
            RichText::new("Downloads")
                .size(34.0)
                .strong()
                .color(theme::TEXT_TITLE),
        );
        ui.label(
            RichText::new(format!(
                "{} active  ·  {} completed  ·  {} failed",
                counts.active, counts.done, counts.failed
            ))
            .size(13.0)
            .color(theme::TEXT_MUTED),
        );
    });
}

fn stats(ui: &mut egui::Ui, entries: &[Download]) {
    let counts = Counts::of(entries);
    let gap = 12.0;
    let width = ((ui.available_width() - gap * 3.0) / 4.0).floor();
    let speed = if counts.speed > 0 {
        format_speed(counts.speed)
    } else {
        "Idle".to_owned()
    };
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        stat_tile(
            ui,
            width,
            "ACTIVE",
            &counts.active.to_string(),
            "active transfers",
        );
        stat_tile(ui, width, "SPEED", &speed, "total speed");
        stat_tile(
            ui,
            width,
            "COMPLETED",
            &counts.done.to_string(),
            "games downloaded",
        );
        stat_tile(ui, width, "FAILED", &counts.failed.to_string(), "to retry");
    });
}

fn stat_tile(ui: &mut egui::Ui, width: f32, caption: &str, value: &str, hint: &str) {
    egui::Frame::new()
        .fill(theme::BG_CARD)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(width - 32.0);
            ui.label(
                RichText::new(caption)
                    .size(10.5)
                    .strong()
                    .color(theme::ACCENT_LIGHT),
            );
            ui.add(
                egui::Label::new(
                    RichText::new(value)
                        .size(20.0)
                        .strong()
                        .color(theme::TEXT_TITLE),
                )
                .truncate(),
            );
            ui.add(
                egui::Label::new(RichText::new(hint).size(11.0).color(theme::TEXT_SUBTLE))
                    .truncate(),
            );
        });
}

fn filter_bar(ui: &mut egui::Ui, filter: &mut Filter) {
    egui::Frame::new()
        .fill(theme::BG_SIDEBAR)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(9)
        .inner_margin(egui::Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for option in Filter::ALL {
                    if segment(ui, option.label(), *filter == option).clicked() {
                        *filter = option;
                    }
                }
            });
        });
}

fn segment(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(12.5),
        Color32::PLACEHOLDER,
    );
    let size = egui::vec2(galley.size().x + 28.0, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("segment-hover"), response.hovered(), 0.1);
    let fill = if selected {
        theme::ACCENT
    } else {
        theme::mix((14, 17, 23), (36, 42, 56), hover)
    };
    let text_color = if selected {
        Color32::WHITE
    } else {
        theme::TEXT_MUTED.lerp_to_gamma(theme::TEXT_TITLE, hover)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 7.0, fill);
    let pos = egui::pos2(
        rect.center().x - galley.size().x / 2.0,
        rect.center().y - galley.size().y / 2.0,
    );
    painter.galley(pos, galley, text_color);
    response
}

// ───────────────────────── Transferts en cours ─────────────────────────

fn active_section(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    epic_catalog: &Catalog,
    images: &mut ImageCache,
    entries: &[Download],
    actions: &mut Vec<(u64, DownloadAction)>,
) {
    // File : le transfert en cours en tête, les suivants dans l'ordre de lancement.
    let mut active: Vec<&Download> = entries.iter().filter(|e| is_active(&e.status)).collect();
    if active.is_empty() {
        return;
    }
    active.sort_by_key(|e| (!is_running(&e.status), e.id));

    section_title(ui, "Active", active.len(), theme::ACCENT_LIGHT);
    for entry in active {
        let game = find_game(catalog, epic_catalog, &entry.name);
        transfer_row(ui, entry, game, images, actions);
        ui.add_space(10.0);
    }
    ui.add_space(14.0);
}

fn transfer_row(
    ui: &mut egui::Ui,
    entry: &Download,
    game: Option<&Game>,
    images: &mut ImageCache,
    actions: &mut Vec<(u64, DownloadAction)>,
) {
    egui::Frame::new()
        .fill(theme::BG_CARD)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(12)
        .inner_margin(egui::Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                cover(ui, game, entry, images, egui::vec2(72.0, 72.0));
                let info_w = (ui.available_width() - 150.0).max(200.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(info_w, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| transfer_info(ui, entry, game),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    transfer_buttons(ui, entry, actions);
                });
            });
        });
}

fn transfer_info(ui: &mut egui::Ui, entry: &Download, game: Option<&Game>) {
    ui.spacing_mut().item_spacing.y = 3.0;
    ui.add(egui::Label::new(RichText::new(&entry.name).size(16.0).strong()).truncate());
    ui.add(egui::Label::new(widgets::muted(download_subtitle(entry, game), 11.0)).truncate());
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        if is_verifying(entry) {
            ui.label(
                RichText::new("Verifying files")
                    .size(12.0)
                    .strong()
                    .color(theme::ACCENT_LIGHT),
            );
        } else {
            ui.label(status_text(&entry.status).size(12.0));
        }
        let line = metrics(entry);
        if !line.is_empty() {
            ui.label(widgets::muted(line, 11.0));
        }
    });
    ui.add_space(6.0);

    if entry.total > 0 {
        let progress = (entry.received as f32 / entry.total as f32).clamp(0.0, 1.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let bar_w = (ui.available_width() - 44.0).max(40.0);
            progress_bar(ui, bar_w, 6.0, progress, bar_color(entry));
            ui.add_sized(
                [34.0, 16.0],
                egui::Label::new(
                    RichText::new(format!("{:.0}%", progress * 100.0))
                        .size(11.0)
                        .color(theme::TEXT_MUTED),
                )
                .halign(egui::Align::RIGHT),
            );
        });
    } else {
        ui.add(
            egui::ProgressBar::new(0.0)
                .desired_height(6.0)
                .corner_radius(3)
                .animate(true),
        );
    }

    // Ubisoft : l'installateur est extrait pendant que les paquets se téléchargent.
    if let Some(percent) = entry.installer_percent {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let bar_w = (ui.available_width() - 200.0).max(40.0);
            progress_bar(ui, bar_w, 3.0, percent as f32 / 100.0, theme::GREEN);
            ui.label(
                RichText::new(format!("Extracting installer  {percent}%"))
                    .size(10.5)
                    .color(theme::TEXT_MUTED),
            );
        });
    }
}

fn transfer_buttons(ui: &mut egui::Ui, entry: &Download, actions: &mut Vec<(u64, DownloadAction)>) {
    ui.spacing_mut().item_spacing.x = 6.0;
    // Ordre de droite à gauche : annuler est le plus à droite.
    if ghost(ui, icons::cancel(15.0), "Cancel", theme::DANGER) {
        actions.push((entry.id, DownloadAction::Cancel));
    }
    if entry.status == DownloadStatus::Paused {
        if ghost(ui, icons::play(15.0), "Resume", theme::ACCENT_LIGHT) {
            actions.push((entry.id, DownloadAction::Resume));
        }
    } else if ghost(ui, icons::pause(15.0), "Pause", theme::TEXT_MUTED) {
        actions.push((entry.id, DownloadAction::Pause));
    }
}

/// Bouton icône sans fond, bordure fine, avec infobulle. Retourne `true` si cliqué.
fn ghost(ui: &mut egui::Ui, image: egui::Image<'static>, tooltip: &str, tint: Color32) -> bool {
    let button = egui::Button::image(image.tint(tint))
        .min_size(egui::vec2(36.0, 36.0))
        .fill(Color32::TRANSPARENT)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(8);
    ui.add(button).on_hover_text(tooltip).clicked()
}

// ───────────────────────── Historique ─────────────────────────

fn done_section(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    epic_catalog: &Catalog,
    images: &mut ImageCache,
    entries: &[Download],
) {
    // Les plus récents d'abord.
    let done: Vec<&Download> = entries
        .iter()
        .filter(|e| e.status == DownloadStatus::Completed)
        .rev()
        .collect();
    if done.is_empty() {
        return;
    }
    section_title(ui, "Completed", done.len(), theme::GREEN);

    let available = ui.available_width();
    let columns =
        (((available + GRID_GAP) / (DONE_MIN_WIDTH + GRID_GAP)).floor() as usize).clamp(1, 3);
    let width = ((available - GRID_GAP * (columns - 1) as f32) / columns as f32).floor();

    for chunk in done.chunks(columns) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = GRID_GAP;
            for entry in chunk {
                let game = find_game(catalog, epic_catalog, &entry.name);
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 1.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(width);
                        done_card(ui, entry, game, images, width);
                    },
                );
            }
        });
        ui.add_space(GRID_GAP);
    }
    ui.add_space(14.0);
}

fn done_card(
    ui: &mut egui::Ui,
    entry: &Download,
    game: Option<&Game>,
    images: &mut ImageCache,
    width: f32,
) {
    egui::Frame::new()
        .fill(theme::BG_CARD)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(12)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(width - 24.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                cover(ui, game, entry, images, egui::vec2(52.0, 52.0));
                let text_w = (width - 24.0 - 52.0 - 12.0 - 44.0).max(120.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(text_w, 0.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.add(
                            egui::Label::new(RichText::new(&entry.name).size(14.0).strong())
                                .truncate(),
                        );
                        ui.add(
                            egui::Label::new(widgets::muted(download_subtitle(entry, game), 11.0))
                                .truncate(),
                        );
                        ui.label(
                            RichText::new(format!("Downloaded · {}", downloaded_size(entry)))
                                .size(11.0)
                                .color(theme::GREEN),
                        );
                        match &entry.registration {
                            Some(Ok(())) => {
                                ui.label(
                                    RichText::new("Registered in Windows")
                                        .size(11.0)
                                        .color(theme::GREEN),
                                );
                            }
                            Some(Err(error)) => {
                                ui.label(
                                    RichText::new(format!(
                                        "Extracted; registration problem: {error}"
                                    ))
                                    .size(11.0)
                                    .color(Color32::from_rgb(248, 191, 90)),
                                );
                            }
                            None => {}
                        }
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ghost(
                        ui,
                        icons::folder_check(15.0),
                        "Open folder",
                        theme::TEXT_MUTED,
                    ) && let Some(parent) = entry.path.parent()
                    {
                        open_folder(parent);
                    }
                });
            });
        });
}

// ───────────────────────── Échecs ─────────────────────────

fn failed_section(ui: &mut egui::Ui, entries: &[Download]) {
    let failed: Vec<(&Download, &String)> = entries
        .iter()
        .rev()
        .filter_map(|e| match &e.status {
            DownloadStatus::Failed(error) => Some((e, error)),
            _ => None,
        })
        .collect();
    if failed.is_empty() {
        return;
    }
    section_title(ui, "Failed", failed.len(), theme::DANGER);

    for (entry, error) in failed {
        egui::Frame::new()
            .fill(theme::BG_CARD)
            .stroke(egui::Stroke::new(1.0, theme::DANGER_DARK))
            .corner_radius(12)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.add(egui::Label::new(RichText::new(&entry.name).size(14.0).strong()).truncate());
                ui.label(widgets::muted("Download failed", 11.0));
                ui.label(
                    RichText::new(error.as_str())
                        .size(12.0)
                        .color(theme::DANGER),
                );
            });
        ui.add_space(10.0);
    }
}

// ───────────────────────── Éléments communs ─────────────────────────

fn section_title(ui: &mut egui::Ui, label: &str, count: usize, color: Color32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        ui.label(
            RichText::new(label.to_uppercase())
                .size(11.0)
                .strong()
                .color(color),
        );
        ui.label(widgets::muted(count.to_string(), 11.0));
    });
    ui.add_space(8.0);
}

fn empty_state(ui: &mut egui::Ui, filter: Filter, nothing_at_all: bool) {
    ui.add_space(70.0);
    ui.vertical_centered(|ui| {
        let (title, text) = if nothing_at_all {
            (
                "No downloads",
                "Downloads started from a game page will appear here.",
            )
        } else {
            (
                "Nothing to show",
                match filter {
                    Filter::Active => "No active transfers right now.",
                    Filter::Done => "No completed games yet.",
                    Filter::Failed => "No failures: everything went well.",
                    Filter::All => "",
                },
            )
        };
        ui.label(
            RichText::new(title)
                .size(18.0)
                .strong()
                .color(theme::TEXT_TITLE),
        );
        ui.add_space(6.0);
        ui.label(RichText::new(text).color(theme::TEXT_MUTED));
    });
}

fn cover(
    ui: &mut egui::Ui,
    game: Option<&Game>,
    entry: &Download,
    images: &mut ImageCache,
    size: egui::Vec2,
) {
    if let Some(game) = game {
        images.request(ui.ctx(), &game.id, &game.image, (480, 270));
    }
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let radius = egui::CornerRadius::same(10);
    match game.and_then(|game| images.get(&game.id)) {
        Some(texture) => images::paint_cover(ui, texture, rect, radius, 1.0),
        None => images::paint_placeholder(ui, rect, radius, &entry.name),
    }
}

/// Barre de progression peinte à la main : fine, arrondie, couleur par statut.
fn progress_bar(ui: &mut egui::Ui, width: f32, height: f32, fraction: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter();
    let radius = height / 2.0;
    painter.rect_filled(rect, radius, Color32::from_rgb(30, 35, 46));
    let filled = egui::Rect::from_min_size(
        rect.min,
        egui::vec2(rect.width() * fraction.clamp(0.0, 1.0), rect.height()),
    );
    if filled.width() > 0.5 {
        painter.rect_filled(filled, radius, color);
    }
}

fn bar_color(entry: &Download) -> Color32 {
    match entry.status {
        DownloadStatus::Paused => theme::GOLD,
        DownloadStatus::Extracting | DownloadStatus::Registering => theme::GREEN,
        _ => theme::ACCENT_LIGHT,
    }
}

fn find_game<'a>(catalog: &'a Catalog, epic: &'a Catalog, name: &str) -> Option<&'a Game> {
    catalog
        .games
        .iter()
        .chain(epic.games.iter())
        .find(|game| game.name.eq_ignore_ascii_case(name))
}

fn download_subtitle(entry: &Download, game: Option<&Game>) -> String {
    let developer = game
        .map(|game| game.developer.as_str())
        .filter(|v| !v.is_empty())
        .unwrap_or("Direct download");
    let path = if entry.registration.is_some() {
        entry.path.parent().unwrap_or(&entry.path)
    } else {
        &entry.path
    };
    format!("{developer}  ·  {}", path.display())
}

fn downloaded_size(entry: &Download) -> String {
    format_size(if entry.downloaded_size > 0 {
        entry.downloaded_size
    } else {
        entry.received
    })
}

/// Ligne de progression textuelle : octets, débit et temps restant.
fn metrics(entry: &Download) -> String {
    let mut parts = Vec::new();
    // Extraction / vérification : `received` est un pourcentage (total = 100).
    if entry.status == DownloadStatus::Extracting && entry.total == 100 {
        parts.push(format!("{}%", entry.received));
    } else if entry.total > 0 {
        parts.push(format!(
            "{} / {}",
            format_size(entry.received),
            format_size(entry.total)
        ));
    } else if entry.received > 0 {
        parts.push(format_size(entry.received));
    }
    if entry.speed > 0 {
        parts.push(format_speed(entry.speed));
    }
    if entry.total > entry.received && entry.speed > 0 {
        parts.push(format!(
            "{} left",
            format_eta(entry.total - entry.received, entry.speed)
        ));
    }
    parts.join("  ·  ")
}

/// Epic : barre unique, les 10 derniers % correspondent à la vérification des fichiers.
fn is_verifying(entry: &Download) -> bool {
    entry.status == DownloadStatus::Downloading
        && entry.store() == GameStore::Epic
        && entry.total > 0
        && entry.received.saturating_mul(10) >= entry.total.saturating_mul(9)
}

fn status_text(status: &DownloadStatus) -> RichText {
    match status {
        DownloadStatus::Queued => widgets::muted("Waiting in queue", 11.0),
        DownloadStatus::Downloading => RichText::new("Downloading")
            .color(theme::ACCENT_LIGHT)
            .strong(),
        DownloadStatus::Extracting => RichText::new("Extracting package")
            .color(theme::ACCENT_LIGHT)
            .strong(),
        DownloadStatus::Registering => RichText::new("Registering in Windows")
            .color(theme::ACCENT_LIGHT)
            .strong(),
        DownloadStatus::Paused => RichText::new("Paused").color(theme::GOLD).strong(),
        DownloadStatus::Completed => RichText::new("Completed").color(theme::GREEN),
        DownloadStatus::Cancelled => widgets::muted("Cancelled", 11.0),
        DownloadStatus::Failed(error) => {
            RichText::new(format!("Failed: {error}")).color(theme::DANGER)
        }
    }
}

fn format_speed(bytes_per_second: u64) -> String {
    if bytes_per_second == 0 {
        return "-- MiB/s".into();
    }
    let units = ["B/s", "KiB/s", "MiB/s", "GiB/s"];
    let exponent = ((bytes_per_second as f64).log(1024.0).floor() as usize).min(3);
    let speed = bytes_per_second as f64 / 1024_f64.powi(exponent as i32);
    format!("{speed:.1} {}", units[exponent])
}

fn format_eta(remaining: u64, bytes_per_second: u64) -> String {
    if bytes_per_second == 0 {
        return "--".into();
    }
    let seconds = remaining.div_ceil(bytes_per_second);
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

/// Le téléchargement occupe actuellement la file (les autres attendent ou sont en pause).
fn is_running(status: &DownloadStatus) -> bool {
    matches!(
        status,
        DownloadStatus::Downloading | DownloadStatus::Extracting | DownloadStatus::Registering
    )
}

fn is_active(status: &DownloadStatus) -> bool {
    matches!(
        status,
        DownloadStatus::Queued
            | DownloadStatus::Downloading
            | DownloadStatus::Paused
            | DownloadStatus::Extracting
            | DownloadStatus::Registering
    )
}
