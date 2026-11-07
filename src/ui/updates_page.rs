//! Vérification de l'application et gestion des mises à jour Xbox.
use super::{icons, theme, widgets};
use crate::library::Library;
use crate::updates::UpdateCheck;
use eframe::egui::{self, Color32, RichText};
use fluent_egui::winui::prelude::*;

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
                row.label(RichText::new(&updates.status).color(theme::TEXT_TITLE));
                if row
                    .add_enabled(!updates.is_loading(), egui::Button::new("Check again"))
                    .clicked()
                {
                    updates.refresh();
                }
            });
            if let Some(release) = &updates.release {
                card.add_space(8.0);
                card.label(
                    RichText::new(format!("{} · v{}", release.name, release.version)).strong(),
                );
                if !release.notes.trim().is_empty() {
                    card.label(
                        RichText::new(release.notes.lines().take(5).collect::<Vec<_>>().join("\n"))
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
