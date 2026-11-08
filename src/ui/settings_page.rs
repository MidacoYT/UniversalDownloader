//! Page « Settings » : dossier de la bibliothèque.
use super::{icons, theme, widgets};
use crate::settings::Settings;
use eframe::egui::{self, Color32, RichText};
use fluent_egui::winui::prelude::*;

const CONTENT_WIDTH: f32 = 760.0;

pub fn show(ui: &mut egui::Ui, settings: &mut Settings) {
    widgets::page_header(
        ui,
        "PREFERENCES",
        "Settings",
        "Manage your app preferences.",
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
                    icons::play(18.0),
                    "Desktop shortcuts",
                    "Like the Xbox and Epic Games apps, add a shortcut to your desktop when a game finishes installing.",
                    |ui| {
                        ui.checkbox(
                            &mut settings.auto_shortcut,
                            "Create a desktop shortcut after each install",
                        );
                    },
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
