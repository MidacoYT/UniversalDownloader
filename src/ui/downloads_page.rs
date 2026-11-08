//! Page des téléchargements : tâches actives et fichiers terminés.
use super::{icons, theme, widgets};
use crate::catalog::Catalog;
use crate::downloads::{Download, DownloadManager, DownloadStatus};
use crate::images::{self, ImageCache};
use crate::model::Game;
use crate::util::{format_size, open_folder};
use eframe::egui::{self, Color32, RichText};
use fluent_egui::winui::prelude::*;

pub fn show(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    epic_catalog: &Catalog,
    images: &mut ImageCache,
    downloads: &mut DownloadManager,
) {
    let active_count = downloads
        .entries
        .iter()
        .filter(|entry| is_active(&entry.status))
        .count();
    widgets::page_header(
        ui,
        "TRANSFERS",
        "Downloads",
        &format!(
            "{active_count} download{} in progress",
            if active_count == 1 { "" } else { "s" }
        ),
    );

    egui::ScrollArea::vertical()
        .id_salt("downloads_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_max_width(850.0);
            if !downloads.notice.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new(&downloads.notice).color(theme::DANGER));
            }
            ui.add_space(18.0);
            active_section(ui, catalog, epic_catalog, images, downloads);
            completed_section(ui, catalog, epic_catalog, images, downloads);
            failed_section(ui, downloads);
        });
}

fn active_section(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    epic_catalog: &Catalog,
    images: &mut ImageCache,
    downloads: &mut DownloadManager,
) {
    let active_count = downloads
        .entries
        .iter()
        .filter(|entry| is_active(&entry.status))
        .count();
    ui.horizontal(|ui| {
        ui.add(icons::download(16.0));
        ui.label(
            RichText::new(format!("In Progress ({active_count})"))
                .size(17.0)
                .strong(),
        );
    });
    ui.add_space(10.0);

    if active_count == 0 {
        egui::Frame::new()
            .fill(theme::BG_CARD)
            .stroke(egui::Stroke::new(1.0, theme::accent_alpha(45)))
            .corner_radius(14)
            .inner_margin(egui::Margin::symmetric(20, 32))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("📭").size(38.0));
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("No active downloads")
                            .strong()
                            .color(theme::TEXT_MUTED),
                    );
                    ui.label(widgets::muted(
                        "Downloads started from a game's page will appear here.",
                        11.0,
                    ));
                });
            });
        return;
    }

    let mut actions = Vec::new();
    for entry in downloads
        .entries
        .iter()
        .rev()
        .filter(|entry| is_active(&entry.status))
    {
        download_card(ui, entry, catalog, epic_catalog, images, &mut actions);
        ui.add_space(10.0);
    }
    for (id, action) in actions {
        match action {
            DownloadAction::Pause => downloads.pause(id),
            DownloadAction::Resume => downloads.resume(id),
            DownloadAction::Cancel => downloads.cancel(id),
        }
    }
}

fn download_card(
    ui: &mut egui::Ui,
    entry: &Download,
    catalog: &Catalog,
    epic_catalog: &Catalog,
    images: &mut ImageCache,
    actions: &mut Vec<(u64, DownloadAction)>,
) {
    let game = catalog
        .games
        .iter()
        .chain(epic_catalog.games.iter())
        .find(|game| game.name.eq_ignore_ascii_case(&entry.name));
    ui.winui(|ui| {
        egui::Frame::new()
            .fill(theme::BG_CARD)
            .stroke(egui::Stroke::new(1.0, theme::accent_alpha(45)))
            .corner_radius(12)
            .inner_margin(egui::Margin::same(16))
            .show(ui, |card| {
                card.set_min_width(card.available_width());
                card.horizontal(|row| {
                    if let Some(game) = game {
                        cover(row, game, images, (76.0, 76.0));
                    } else {
                        let (rect, _) = row.allocate_exact_size(egui::vec2(76.0, 76.0), egui::Sense::hover());
                        row.painter()
                            .rect_filled(rect, 10.0, theme::accent_alpha(45));
                        row.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "↓",
                            egui::FontId::proportional(24.0),
                            theme::ACCENT_LIGHT,
                        );
                    }

                    row.vertical(|details| {
                        details.set_min_width(details.available_width());
                        details.label(RichText::new(&entry.name).size(17.0).strong());
                        details.label(widgets::muted(download_subtitle(entry, game), 12.0));
                        details.add_space(5.0);
                        details.horizontal(|line| {
                            line.add(icons::download(13.0));
                            line.label(status_text(&entry.status));
                            let mut parts = Vec::new();
                            if entry.total > 0 {
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
                            if !parts.is_empty() {
                                line.label(widgets::muted(parts.join("  ·  "), 11.0));
                            }
                        });
                        details.add_space(5.0);
                        if entry.total > 0 {
                            let progress =
                                (entry.received as f32 / entry.total as f32).clamp(0.0, 1.0);
                            details.horizontal(|progress_row| {
                                let percent_width = 42.0;
                                let bar_width =
                                    (progress_row.available_width() - percent_width).max(24.0);
                                // `desired_height` : `add_sized` ne réduit pas la barre, elle
                                // garde sa hauteur par défaut.
                                progress_row.add(
                                    egui::ProgressBar::new(progress)
                                        .desired_width(bar_width)
                                        .desired_height(4.0)
                                        .corner_radius(2)
                                        .fill(if entry.status == DownloadStatus::Paused {
                                            theme::GOLD
                                        } else {
                                            theme::ACCENT_LIGHT
                                        }),
                                );
                                progress_row.add_sized(
                                    [percent_width, 18.0],
                                    egui::Label::new(
                                        RichText::new(format!("{:.0}%", progress * 100.0))
                                            .size(11.0)
                                            .color(theme::TEXT_MUTED),
                                    )
                                    .halign(egui::Align::RIGHT),
                                );
                            });
                        } else {
                            details.add(
                                egui::ProgressBar::new(0.0)
                                    .desired_height(4.0)
                                    .corner_radius(2)
                                    .animate(true),
                            );
                        }
                        details.horizontal(|buttons| match entry.status {
                            DownloadStatus::Extracting | DownloadStatus::Registering => {
                                if buttons
                                    .add(egui::Button::image_and_text(icons::pause(12.0), "Pause"))
                                    .clicked()
                                {
                                    actions.push((entry.id, DownloadAction::Pause));
                                }
                                if buttons
                                    .add(egui::Button::image_and_text(
                                        icons::cancel(12.0),
                                        "Cancel",
                                    ))
                                    .clicked()
                                {
                                    actions.push((entry.id, DownloadAction::Cancel));
                                }
                            }
                            DownloadStatus::Paused => {
                                if buttons
                                    .add(egui::Button::image_and_text(icons::play(12.0), "Resume"))
                                    .clicked()
                                {
                                    actions.push((entry.id, DownloadAction::Resume));
                                }
                                if buttons
                                    .add(egui::Button::image_and_text(
                                        icons::cancel(12.0),
                                        "Cancel",
                                    ))
                                    .clicked()
                                {
                                    actions.push((entry.id, DownloadAction::Cancel));
                                }
                            }
                            _ => {
                                if buttons
                                    .add(egui::Button::image_and_text(icons::pause(12.0), "Pause"))
                                    .clicked()
                                {
                                    actions.push((entry.id, DownloadAction::Pause));
                                }
                                if buttons
                                    .add(egui::Button::image_and_text(
                                        icons::cancel(12.0),
                                        "Cancel",
                                    ))
                                    .clicked()
                                {
                                    actions.push((entry.id, DownloadAction::Cancel));
                                }
                            }
                        });
                    });
                });
            });
    });
}

fn completed_section(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    epic_catalog: &Catalog,
    images: &mut ImageCache,
    downloads: &DownloadManager,
) {
    let completed: Vec<_> = downloads
        .entries
        .iter()
        .filter(|entry| entry.status == DownloadStatus::Completed)
        .collect();
    if completed.is_empty() {
        return;
    }
    ui.add_space(16.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new("✓").color(theme::GREEN).strong());
        ui.label(
            RichText::new(format!("Completed ({})", completed.len()))
                .size(17.0)
                .strong(),
        );
    });
    ui.add_space(10.0);
    for entry in completed.into_iter().rev() {
        let game = catalog
            .games
            .iter()
            .chain(epic_catalog.games.iter())
            .find(|game| game.name.eq_ignore_ascii_case(&entry.name));
        ui.winui(|ui| {
            egui::Frame::new()
                .fill(theme::BG_CARD)
                .stroke(egui::Stroke::new(1.0, theme::accent_alpha(45)))
                .corner_radius(12)
                .inner_margin(egui::Margin::same(14))
                .show(ui, |card| {
                card.set_min_width(card.available_width());
                card.horizontal(|row| {
                    if let Some(game) = game {
                        cover(row, game, images, (60.0, 60.0));
                    } else {
                        let (rect, _) =
                            row.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::hover());
                        row.painter()
                            .rect_filled(rect, 8.0, theme::accent_alpha(45));
                        row.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "✓",
                            egui::FontId::proportional(18.0),
                            theme::GREEN,
                        );
                    }
                    row.vertical(|details| {
                        details.label(RichText::new(&entry.name).size(15.0).strong());
                        details.label(widgets::muted(download_subtitle(entry, game), 11.0));
                        details.label(
                            RichText::new(format!(
                                "Downloaded · {}",
                                format_size(if entry.downloaded_size > 0 {
                                    entry.downloaded_size
                                } else {
                                    entry.received
                                })
                            ))
                            .size(11.0)
                            .color(theme::GREEN),
                        );
                        match &entry.registration {
                            Some(Ok(())) => {
                                details.label(
                                    RichText::new("Registered with Windows")
                                        .size(11.0)
                                        .color(theme::GREEN),
                                );
                            }
                            Some(Err(error)) => {
                                details.label(
                                    RichText::new(format!(
                                        "Extracted; registration issue: {error}"
                                    ))
                                    .size(11.0)
                                    .color(Color32::from_rgb(248, 191, 90)),
                                );
                            }
                            None => {}
                        }
                        if details.add(egui::Button::image_and_text(icons::folder_check(13.0), "Open folder")).clicked()
                            && let Some(parent) = entry.path.parent()
                        {
                            open_folder(parent);
                        }
                    });
                });
            });
        });
        ui.add_space(8.0);
    }
}

fn failed_section(ui: &mut egui::Ui, downloads: &DownloadManager) {
    let failed: Vec<_> = downloads
        .entries
        .iter()
        .filter(|entry| matches!(entry.status, DownloadStatus::Failed(_)))
        .collect();
    if failed.is_empty() {
        return;
    }
    ui.add_space(16.0);
    ui.label(
        RichText::new(format!("Failed ({})", failed.len()))
            .size(17.0)
            .strong(),
    );
    ui.add_space(8.0);
    for entry in failed.into_iter().rev() {
        if let DownloadStatus::Failed(error) = &entry.status {
            ui.winui(|ui| {
                ui.card(&entry.name, "Download failed", |card| {
                    card.label(RichText::new(error).color(Color32::from_rgb(248, 113, 113)));
                });
            });
            ui.add_space(8.0);
        }
    }
}

fn cover(ui: &mut egui::Ui, game: &Game, images: &mut ImageCache, size: (f32, f32)) {
    images.request(ui.ctx(), &game.id, &game.image, (120, 120));
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size.0, size.1), egui::Sense::hover());
    match images.get(&game.id) {
        Some(texture) => images::paint_cover(ui, texture, rect, egui::CornerRadius::same(10), 1.0),
        None => {
            images::paint_placeholder(ui, rect, egui::CornerRadius::same(10), game.display_name())
        }
    }
}

fn download_subtitle(entry: &Download, game: Option<&Game>) -> String {
    let developer = game
        .map(|game| game.developer.as_str())
        .unwrap_or("Direct download");
    let path = if entry.registration.is_some() {
        entry.path.parent().unwrap_or(&entry.path)
    } else {
        &entry.path
    };
    format!("{developer} · {}", path.display())
}

fn status_text(status: &DownloadStatus) -> RichText {
    match status {
        DownloadStatus::Queued => widgets::muted("Waiting in queue", 11.0),
        DownloadStatus::Downloading => RichText::new("Downloading in progress")
            .color(theme::ACCENT_LIGHT)
            .strong(),
        DownloadStatus::Extracting => RichText::new("Extracting package")
            .color(theme::ACCENT_LIGHT)
            .strong(),
        DownloadStatus::Registering => RichText::new("Registering game with Windows")
            .color(theme::ACCENT_LIGHT)
            .strong(),
        DownloadStatus::Paused => RichText::new("Paused").color(theme::GOLD).strong(),
        DownloadStatus::Completed => RichText::new("Completed").color(theme::GREEN),
        DownloadStatus::Cancelled => widgets::muted("Cancelled", 11.0),
        DownloadStatus::Failed(error) => {
            RichText::new(format!("Failed: {error}")).color(Color32::from_rgb(248, 113, 113))
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

#[derive(Clone, Copy)]
enum DownloadAction {
    Pause,
    Resume,
    Cancel,
}
