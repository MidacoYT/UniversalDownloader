//! Page « Settings » : dossier de la bibliothèque.
use super::{icons, theme, widgets};
use crate::settings::Settings;
use crate::util::open_folder;
use eframe::egui::{self, Color32, RichText};
use fluent_egui::winui::prelude::*;
use std::path::Path;

const CONTENT_WIDTH: f32 = 760.0;
const FIELD_HEIGHT: f32 = 32.0;

pub fn show(ui: &mut egui::Ui, settings: &mut Settings) {
    widgets::page_header(
        ui,
        "PREFERENCES",
        "Settings",
        "Choose where your local game library is stored.",
    );
    egui::ScrollArea::vertical()
        .id_salt("settings_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.winui(|ui| {
                ui.set_max_width(CONTENT_WIDTH);
                ui.spacing_mut().item_spacing.y = 12.0;

                section(
                    ui,
                    icons::folder(18.0),
                    "Game library folder",
                    "Scanned to find your locally installed games. Rescan from My Library after changing it.",
                    |ui| folder_row(ui, &mut settings.library_dir, "Choose a game library folder"),
                );
                ui.add_space(4.0);
                actions(ui, settings);
                ui.add_space(6.0);
                ui.label(widgets::muted(
                    format!("Universal Downloader v{}", env!("CARGO_PKG_VERSION")),
                    10.0,
                ));
                ui.add_space(16.0);
            });
        });
}

/// Carte de section : pastille d'icône, titre, description, puis le contenu.
fn section(
    ui: &mut egui::Ui,
    image: egui::Image<'static>,
    title: &str,
    description: &str,
    body: impl FnOnce(&mut egui::Ui),
) {
    egui::Frame::new()
        .fill(theme::BG_CARD)
        .stroke(egui::Stroke::new(1.0, theme::accent_alpha(45)))
        .corner_radius(12)
        .inner_margin(egui::Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (tile, _) =
                    ui.allocate_exact_size(egui::vec2(38.0, 38.0), egui::Sense::hover());
                ui.painter().rect_filled(tile, 10, theme::accent_alpha(32));
                image.paint_at(
                    ui,
                    egui::Rect::from_center_size(tile.center(), egui::vec2(18.0, 18.0)),
                );
                ui.add_space(6.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(title).size(15.0).strong());
                    ui.label(widgets::muted(description, 11.5));
                });
            });
            ui.add_space(14.0);
            body(ui);
        });
}

/// Champ de chemin pleine largeur + boutons Browse / Open, puis état du dossier.
fn folder_row(ui: &mut egui::Ui, value: &mut String, hint: &str) {
    const BROWSE_W: f32 = 88.0;
    const OPEN_W: f32 = 64.0;
    ui.horizontal(|ui| {
        let spacing = ui.spacing().item_spacing.x;
        let field_w = (ui.available_width() - BROWSE_W - OPEN_W - spacing * 2.0).max(120.0);
        ui.add_sized(
            [field_w, FIELD_HEIGHT],
            egui::TextEdit::singleline(value)
                .hint_text(hint)
                .vertical_align(egui::Align::Center),
        );
        if ui
            .add_sized([BROWSE_W, FIELD_HEIGHT], egui::Button::new("Browse…"))
            .clicked()
        {
            if let Some(path) = rfd::FileDialog::new().set_directory(&*value).pick_folder() {
                *value = path.display().to_string();
            }
        }
        let exists = Path::new(value.trim()).is_dir();
        if ui
            .add_enabled(
                exists,
                egui::Button::new("Open").min_size(egui::vec2(OPEN_W, FIELD_HEIGHT)),
            )
            .clicked()
        {
            open_folder(Path::new(value.trim()));
        }
    });

    let trimmed = value.trim();
    let (color, text, found) = if trimmed.is_empty() {
        (Color32::GRAY, "No folder selected", false)
    } else if Path::new(trimmed).is_dir() {
        (theme::GREEN, "Folder found", true)
    } else {
        (Color32::from_rgb(248, 113, 113), "Folder not found", false)
    };
    ui.horizontal(|ui| {
        if found {
            ui.add(icons::folder_check(13.0));
        } else {
            ui.label(RichText::new("●").size(8.0).color(color));
        }
        ui.label(RichText::new(text).size(11.0).color(color));
    });
}

fn actions(ui: &mut egui::Ui, settings: &mut Settings) {
    ui.horizontal(|ui| {
        if ui.add(FluentButton::accent("Save settings")).clicked() {
            settings.save();
        }
        if ui.button("Reset to defaults").clicked() {
            settings.reset();
        }
        if !settings.status.is_empty() {
            ui.add_space(6.0);
            ui.add(icons::check(13.0));
            ui.label(
                RichText::new(&settings.status)
                    .size(11.5)
                    .color(Color32::from_rgb(110, 231, 183)),
            );
        }
    });
}
