//! Update page for the application and Xbox games.
use super::{icons, theme, widgets};
use crate::library::Library;
use crate::updates::UpdateCheck;
use eframe::egui::{self, Color32, RichText};
use fluent_egui::winui::prelude::*;

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

pub fn show(ui: &mut egui::Ui, library: &Library, updates: &mut UpdateCheck) {
    widgets::page_header(
        ui,
        "MAINTENANCE",
        "Updates",
        "Install app updates from GitHub and manage Xbox game updates through Microsoft Store.",
    );
    let mut install_update = false;
    ui.winui(|ui| {
        ui.add_space(8.0);
        ui.card("Universal Downloader", "Application release", |card| {
            card.horizontal(|row| {
                row.add(icons::calendar(18.0));
                row.vertical(|details| {
                    let current = env!("CARGO_PKG_VERSION").trim_start_matches('v');
                    details.label(
                        RichText::new(format!("Installed version: v{current}"))
                            .size(12.0)
                            .color(theme::TEXT_MUTED),
                    );
                    if let Some(release) = &updates.release {
                        details.label(
                            RichText::new(format!("Latest version: v{}", release.version))
                                .strong()
                                .color(theme::TEXT_TITLE),
                        );
                    }
                    details.label(RichText::new(&updates.status).color(theme::TEXT_TITLE));
                });
                if row
                    .add_enabled(!updates.is_loading(), egui::Button::new("Check again"))
                    .clicked()
                {
                    updates.refresh();
                }
            });
            if let Some((received, total)) = updates.install_progress {
                card.add_space(8.0);
                let fraction = total
                    .filter(|total| *total > 0)
                    .map(|total| (received as f32 / total as f32).clamp(0.0, 1.0));
                card.add(egui::ProgressBar::new(fraction.unwrap_or(0.0)).animate(fraction.is_none()));
                let progress_text = match total.filter(|total| *total > 0) {
                    Some(total) => format!(
                        "{}% · {:.1} / {:.1} MB",
                        (received.saturating_mul(100) / total).min(100),
                        received as f64 / 1_048_576.0,
                        total as f64 / 1_048_576.0
                    ),
                    None => format!("Downloaded {:.1} MB", received as f64 / 1_048_576.0),
                };
                card.label(RichText::new(progress_text).size(11.0).color(Color32::GRAY));
            }
            if let Some(release) = &updates.release {
                card.add_space(8.0);
                let standard_name = format!("Universal Downloader v{}", release.version);
                if !release.name.trim().eq_ignore_ascii_case(&standard_name) {
                    card.label(RichText::new(&release.name).strong());
                }
                if let Some(notes) = release_summary(&release.notes) {
                    card.label(RichText::new(notes).size(11.0).color(Color32::GRAY));
                } else {
                    card.label(
                        RichText::new(
                            "The installer and SHA-256 checksum are available on the release page.",
                        )
                        .size(11.0)
                        .color(Color32::GRAY),
                    );
                }
                card.horizontal(|row| {
                    if release.update_available
                        && release.installer_url.is_some()
                        && release.installer_sha256.is_some()
                    {
                        let button_text = if updates.is_loading() {
                            "Installing update…"
                        } else {
                            "Download and install update"
                        };
                        if row
                            .add_enabled(
                                !updates.is_loading(),
                                FluentButton::accent(button_text),
                            )
                            .clicked()
                        {
                            install_update = true;
                        }
                    }
                    if row.button("Open release page").clicked() {
                        let _ = webbrowser::open(&release.url);
                    }
                });
            }
        });
        ui.add_space(12.0);
        ui.card(
            "Xbox game updates",
            "Installed games: Microsoft Store manages their updates and licenses.",
            |card| {
                card.label(format!(
                    "{} game folders are currently in your library.",
                    library.entries.len()
                ));
                card.add_space(6.0);
                card.label(
                    RichText::new("Universal Downloader does not have an official Xbox package version feed, so it cannot reliably report game updates here.")
                        .size(11.0)
                        .color(Color32::GRAY),
                );
                card.add_space(8.0);
                if card
                    .add(FluentButton::accent("Open Microsoft Store updates"))
                    .clicked()
                {
                    let _ = webbrowser::open("ms-windows-store://downloadsandupdates");
                }
            },
        );
    });
    if install_update {
        updates.install_update();
    }
}
