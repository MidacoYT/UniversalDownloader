//! Mises à jour : version de l'application (comparaison, notes, installation) et
//! rappel des mises à jour des jeux Xbox, gérées par le Microsoft Store.
use super::{theme, widgets};
use crate::library::Library;
use crate::updates::UpdateCheck;
use eframe::egui::{self, Color32, RichText};

const GAP: f32 = 16.0;

pub fn show(ui: &mut egui::Ui, library: &Library, updates: &mut UpdateCheck) {
    let current = env!("CARGO_PKG_VERSION").trim_start_matches('v').to_owned();
    let mut install_update = false;

    header(ui);
    ui.add_space(18.0);
    stats(ui, library, updates, &current);
    ui.add_space(20.0);

    egui::ScrollArea::vertical()
        .id_salt("updates_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            install_update = app_card(ui, updates, &current);
            ui.add_space(GAP);
            xbox_card(ui, library);
            ui.add_space(40.0);
        });

    if install_update {
        updates.install_update();
    }
}

// ───────────────────────── En-tête et statistiques ─────────────────────────

fn header(ui: &mut egui::Ui) {
    ui.add_space(24.0);
    ui.vertical(|ui| {
        ui.label(
            RichText::new("Updates")
                .size(34.0)
                .strong()
                .color(theme::TEXT_TITLE),
        );
        ui.label(
            RichText::new("Application and Xbox games")
                .size(13.0)
                .color(theme::TEXT_MUTED),
        );
    });
}

fn stats(ui: &mut egui::Ui, library: &Library, updates: &UpdateCheck, current: &str) {
    let latest = updates
        .release
        .as_ref()
        .map(|r| format!("v{}", r.version.trim_start_matches('v')))
        .unwrap_or_else(|| "—".to_owned());
    let state = state_label(updates);
    let games = library.entries.len().to_string();

    let gap = 12.0;
    let width = ((ui.available_width() - gap * 3.0) / 4.0).floor();
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        stat_tile(
            ui,
            width,
            "INSTALLED VERSION",
            &format!("v{current}"),
            "application",
        );
        stat_tile(ui, width, "LATEST VERSION", &latest, "on GitHub");
        stat_tile(ui, width, "STATUS", state, "checking the application");
        stat_tile(ui, width, "GAMES", &games, "in library");
    });
}

fn state_label(updates: &UpdateCheck) -> &'static str {
    if updates.is_loading() {
        "Checking…"
    } else {
        match &updates.release {
            Some(release) if release.update_available => "Available",
            Some(_) => "Up to date",
            None => "Unknown",
        }
    }
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

// ───────────────────────── Carte application ─────────────────────────

/// Retourne `true` si l'utilisateur demande l'installation de la mise à jour.
fn app_card(ui: &mut egui::Ui, updates: &mut UpdateCheck, current: &str) -> bool {
    let mut install = false;
    let loading = updates.is_loading();

    card(ui, |ui| {
        ui.label(
            RichText::new("Universal Downloader")
                .size(18.0)
                .strong()
                .color(theme::TEXT_TITLE),
        );
        ui.add_space(14.0);

        // Comparaison des deux versions.
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            version_block(ui, "Installed", &format!("v{current}"), theme::TEXT_TITLE);
            ui.label(RichText::new("→").size(20.0).color(theme::TEXT_SUBTLE));
            match &updates.release {
                Some(release) => {
                    let color = if release.update_available {
                        theme::GREEN
                    } else {
                        theme::TEXT_TITLE
                    };
                    let latest = format!("v{}", release.version.trim_start_matches('v'));
                    version_block(ui, "Latest", &latest, color);
                }
                None => version_block(ui, "Latest", "—", theme::TEXT_MUTED),
            }
        });
        ui.add_space(14.0);

        // Statut et progression de l'installation.
        ui.label(
            RichText::new(&updates.status)
                .size(12.5)
                .color(theme::TEXT_MUTED),
        );
        if let Some((received, total)) = updates.install_progress {
            ui.add_space(8.0);
            let known = total.filter(|t| *t > 0);
            match known {
                Some(total) => {
                    let fraction = (received as f32 / total as f32).clamp(0.0, 1.0);
                    let width = ui.available_width();
                    progress_bar(ui, width, 6.0, fraction, theme::ACCENT_LIGHT);
                    ui.label(
                        RichText::new(format!(
                            "{}%  ·  {:.1} / {:.1} MB",
                            (received.saturating_mul(100) / total).min(100),
                            received as f64 / 1_048_576.0,
                            total as f64 / 1_048_576.0
                        ))
                        .size(11.0)
                        .color(theme::TEXT_SUBTLE),
                    );
                }
                None => {
                    ui.add(
                        egui::ProgressBar::new(0.0)
                            .animate(true)
                            .desired_height(6.0),
                    );
                    ui.label(
                        RichText::new(format!(
                            "Downloaded: {:.1} MB",
                            received as f64 / 1_048_576.0
                        ))
                        .size(11.0)
                        .color(theme::TEXT_SUBTLE),
                    );
                }
            }
        }

        // Notes de version de la dernière release.
        if let Some(release) = &updates.release {
            ui.add_space(16.0);
            let standard = format!("Universal Downloader v{}", release.version);
            if !release.name.trim().eq_ignore_ascii_case(&standard) {
                ui.label(
                    RichText::new(&release.name)
                        .strong()
                        .color(theme::TEXT_TITLE),
                );
            }
            let notes = release_summary(&release.notes).unwrap_or_else(|| {
                "The installer and SHA-256 checksum are available on the release page.".to_owned()
            });
            egui::Frame::new()
                .fill(theme::BG_APP)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .corner_radius(8)
                .inner_margin(egui::Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(
                        egui::Label::new(RichText::new(notes).size(12.0).color(theme::TEXT_MUTED))
                            .wrap(),
                    );
                });
        }

        // Actions.
        ui.add_space(16.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
            if let Some(release) = &updates.release {
                let installable = release.update_available
                    && release.installer_url.is_some()
                    && release.installer_sha256.is_some();
                if installable {
                    let text = if loading {
                        "Installing…"
                    } else {
                        "Download and install"
                    };
                    if primary_button(ui, text, !loading) {
                        install = true;
                    }
                }
                if secondary_button(ui, "Open release page", true) {
                    let _ = webbrowser::open(&release.url);
                }
            }
            if secondary_button(ui, "Check again", !loading) {
                updates.refresh();
            }
        });
    });
    install
}

fn version_block(ui: &mut egui::Ui, caption: &str, value: &str, color: Color32) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(
            RichText::new(caption.to_uppercase())
                .size(10.0)
                .strong()
                .color(theme::TEXT_SUBTLE),
        );
        ui.label(RichText::new(value).size(22.0).strong().color(color));
    });
}

// ───────────────────────── Carte Xbox ─────────────────────────

fn xbox_card(ui: &mut egui::Ui, library: &Library) {
    card(ui, |ui| {
        ui.label(
            RichText::new("Xbox games")
                .size(18.0)
                .strong()
                .color(theme::TEXT_TITLE),
        );
        ui.add_space(4.0);
        ui.label(widgets::muted(
            "Microsoft Store manages updates and licenses for these games.",
            12.0,
        ));
        ui.add_space(12.0);
        ui.label(
            RichText::new(format!(
                "{} game folder(s) in your library.",
                library.entries.len()
            ))
            .size(13.0)
            .color(theme::TEXT_TITLE),
        );
        ui.add_space(6.0);
        ui.add(
            egui::Label::new(widgets::muted(
                "Universal Downloader has no official feed of Xbox package versions, so it cannot reliably show these updates.",
                11.5,
            ))
            .wrap(),
        );
        ui.add_space(14.0);
        if primary_button(ui, "Open Microsoft Store updates", true) {
            let _ = webbrowser::open("ms-windows-store://downloadsandupdates");
        }
    });
}

// ───────────────────────── Éléments communs ─────────────────────────

fn card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(theme::BG_CARD)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(12)
        .inner_margin(egui::Margin::same(22))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

fn primary_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> bool {
    let button = egui::Button::new(RichText::new(text).strong().color(Color32::WHITE))
        .fill(theme::ACCENT)
        .stroke(egui::Stroke::NONE)
        .corner_radius(8)
        .min_size(egui::vec2(0.0, 38.0));
    ui.add_enabled(enabled, button).clicked()
}

fn secondary_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> bool {
    let button = egui::Button::new(RichText::new(text).color(theme::TEXT_TITLE))
        .fill(theme::BG_CARD_HOVER)
        .stroke(egui::Stroke::new(1.0, theme::BORDER_STRONG))
        .corner_radius(8)
        .min_size(egui::vec2(0.0, 38.0));
    ui.add_enabled(enabled, button).clicked()
}

/// Barre de progression peinte à la main : fine, arrondie.
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

/// Retire les lignes génériques des notes (commit, lien d'installation).
fn release_summary(notes: &str) -> Option<String> {
    let summary = notes
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with("Windows installer built from commit "))
        .filter(|line| !line.starts_with("Download `UniversalDownloader-Setup.exe`"))
        .collect::<Vec<_>>()
        .join("\n");
    (!summary.is_empty()).then_some(summary)
}
