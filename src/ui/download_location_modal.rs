//! Install-location prompt opened before starting a game download.
//! Centered card with the game and store, a destination field with a live preview of the
//! final game folder, a validation line, and right-aligned actions.
use super::{icons, theme};
use crate::model::GameStore;
use eframe::egui::{self, Color32, RichText};
use std::path::Path;

const WIDTH: f32 = 560.0;
const HEIGHT: f32 = 340.0;

pub fn show(
    ctx: &egui::Context,
    name: &str,
    store: GameStore,
    directory: &mut String,
    generation: u64,
    out: &mut Vec<crate::ui::Action>,
) {
    let id = egui::Id::new(("download_location", generation));
    let progress = ctx.animate_bool_with_time(id.with("open"), true, 0.22);
    let eased = 1.0 - (1.0 - progress).powi(3);

    let content = ctx.content_rect();
    // Centered in the content area; follows window resizes. Slides up slightly on open.
    let width = WIDTH.min(content.width() - 48.0).max(320.0);
    let rect = egui::Rect::from_center_size(
        content.center() + egui::vec2(0.0, 18.0 * (1.0 - eased)),
        egui::vec2(width, HEIGHT),
    );

    ctx.layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::new(("download_location_backdrop", generation)),
    ))
    .rect_filled(
        content,
        0.0,
        Color32::from_black_alpha((140.0 * eased) as u8),
    );

    let mut confirm = false;
    let mut dismiss = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    let trimmed = directory.trim().to_owned();
    let valid = Path::new(&trimmed).is_absolute();

    egui::Area::new(id.with("area"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme::BG_MODAL)
                .stroke(egui::Stroke::new(1.0, theme::BORDER_STRONG))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 12],
                    blur: 42,
                    spread: 0,
                    color: Color32::from_rgba_unmultiplied(0, 0, 0, 180),
                })
                .corner_radius(egui::CornerRadius::same(12))
                .inner_margin(egui::Margin::same(24))
                .show(ui, |ui| {
                    ui.set_width(width - 48.0);

                    // Header: kicker, title, close button.
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(
                                RichText::new("DOWNLOAD")
                                    .size(10.5)
                                    .strong()
                                    .color(theme::ACCENT_LIGHT),
                            );
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!("Install {name}"))
                                        .size(20.0)
                                        .strong()
                                        .color(theme::TEXT_TITLE),
                                )
                                .truncate(),
                            );
                        });
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Min),
                            |ui| {
                                let close = egui::Button::new(
                                    RichText::new("×").size(16.0).color(theme::TEXT_MUTED),
                                )
                                .min_size(egui::vec2(32.0, 32.0))
                                .fill(Color32::TRANSPARENT)
                                .stroke(egui::Stroke::NONE)
                                .corner_radius(8);
                                if ui.add(close).on_hover_text("Close").clicked() {
                                    dismiss = true;
                                }
                            },
                        );
                    });
                    ui.add_space(12.0);

                    // Store chip.
                    let (store_label, store_color) = match store {
                        GameStore::Xbox => ("XBOX GAME PASS", theme::GREEN),
                        GameStore::Epic => ("EPIC GAMES", theme::EPIC_BLUE),
                    };
                    egui::Frame::new()
                        .fill(theme::BG_APP)
                        .stroke(egui::Stroke::new(1.0, theme::BORDER))
                        .corner_radius(6)
                        .inner_margin(egui::Margin::symmetric(10, 4))
                        .show(ui, |ui| {
                            ui.label(RichText::new(store_label).size(10.5).strong().color(store_color));
                        });
                    ui.add_space(20.0);

                    // Destination field and browse button.
                    ui.label(
                        RichText::new("Destination folder")
                            .size(12.0)
                            .strong()
                            .color(theme::TEXT_MUTED),
                    );
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let field_w = (ui.available_width() - 120.0).max(120.0);
                        ui.add(
                            egui::TextEdit::singleline(directory)
                                .hint_text("C:\\Games")
                                .desired_width(field_w)
                                .margin(egui::vec2(12.0, 9.0)),
                        );
                        let browse = egui::Button::image_and_text(
                            icons::folder(15.0).tint(theme::TEXT_MUTED),
                            RichText::new("Browse…").color(theme::TEXT_TITLE),
                        )
                        .fill(theme::BG_CARD_HOVER)
                        .stroke(egui::Stroke::new(1.0, theme::BORDER_STRONG))
                        .corner_radius(8)
                        .min_size(egui::vec2(112.0, 36.0));
                        if ui.add(browse).clicked() {
                            let mut picker =
                                rfd::FileDialog::new().set_title("Choose the install folder");
                            if Path::new(&trimmed).is_dir() {
                                picker = picker.set_directory(&trimmed);
                            }
                            if let Some(path) = picker.pick_folder() {
                                *directory = path.display().to_string();
                            }
                        }
                    });
                    ui.add_space(8.0);

                    // Validation or live preview of the final folder.
                    if trimmed.is_empty() {
                        ui.label(
                            RichText::new("Choose a folder to continue.")
                                .size(11.5)
                                .color(theme::TEXT_SUBTLE),
                        );
                    } else if !valid {
                        ui.label(
                            RichText::new("Enter a full path, such as C:\\Games.")
                                .size(11.5)
                                .color(theme::DANGER),
                        );
                    } else {
                        let preview = Path::new(&trimmed).join(name);
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("Game folder: {}", preview.display()))
                                    .size(11.5)
                                    .color(theme::TEXT_MUTED),
                            )
                            .truncate(),
                        );
                    }
                    ui.add_space(14.0);

                    // What happens next, depending on the store.
                    let note = match store {
                        GameStore::Xbox => "The Xbox package will be downloaded into this folder.",
                        GameStore::Epic => "The Epic game files will be installed into this folder.",
                    };
                    egui::Frame::new()
                        .fill(theme::BG_APP)
                        .stroke(egui::Stroke::new(1.0, theme::BORDER))
                        .corner_radius(8)
                        .inner_margin(egui::Margin::symmetric(14, 10))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(note).size(12.0).color(theme::TEXT_MUTED));
                        });
                    ui.add_space(22.0);

                    // Actions, right-aligned: Download (primary) then Cancel.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        let download = egui::Button::new(
                            RichText::new("Download").strong().color(Color32::WHITE),
                        )
                        .fill(theme::ACCENT)
                        .stroke(egui::Stroke::NONE)
                        .corner_radius(8)
                        .min_size(egui::vec2(124.0, 38.0));
                        if ui.add_enabled(valid, download).clicked() {
                            confirm = true;
                        }
                        let cancel = egui::Button::new(
                            RichText::new("Cancel").color(theme::TEXT_TITLE),
                        )
                        .fill(theme::BG_CARD_HOVER)
                        .stroke(egui::Stroke::new(1.0, theme::BORDER_STRONG))
                        .corner_radius(8)
                        .min_size(egui::vec2(104.0, 38.0));
                        if ui.add(cancel).clicked() {
                            dismiss = true;
                        }
                    });
                });
        });

    if confirm {
        out.push(crate::ui::Action::ConfirmDownload {
            directory: trimmed,
        });
    } else if dismiss {
        out.push(crate::ui::Action::DismissDownload);
    }
}
