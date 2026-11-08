//! Install-location prompt opened before starting a game download.
use super::Action;
use crate::model::GameStore;
use eframe::egui;
use std::path::Path;

pub fn show(
    ctx: &egui::Context,
    name: &str,
    store: GameStore,
    directory: &mut String,
    generation: u64,
    out: &mut Vec<Action>,
) {
    let mut open = true;
    let mut confirm = false;
    let mut dismiss = false;

    let id = egui::Id::new(("download_location", generation));
    let progress = ctx.animate_bool_with_time(id.with("open_animation"), true, 0.24);
    let eased = 1.0 - (1.0 - progress).powi(3);
    let size = egui::vec2(560.0, 240.0);
    let center = ctx.content_rect().center();
    let position = center - size * 0.5 + egui::vec2(0.0, 18.0 * (1.0 - eased));

    egui::Window::new("Choose install location")
        .id(id)
        .collapsible(false)
        .resizable(false)
        .fixed_size(size)
        .fixed_pos(position)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.set_width(520.0);
            ui.label(
                egui::RichText::new(format!("Install {name}"))
                    .size(16.0)
                    .strong(),
            );
            ui.add_space(8.0);
            ui.label("Choose the parent folder. Universal Downloader will create a game-named folder inside it.");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_sized(
                    [ui.available_width() - 92.0, 32.0],
                    egui::TextEdit::singleline(directory)
                        .hint_text("Folder path")
                        .vertical_align(egui::Align::Center),
                );
                if ui.add_sized([84.0, 32.0], egui::Button::new("Browse…")).clicked() {
                    let mut picker = rfd::FileDialog::new().set_title("Choose install folder");
                    if Path::new(directory.trim()).is_dir() {
                        picker = picker.set_directory(directory.trim());
                    }
                    if let Some(path) = picker.pick_folder() {
                        *directory = path.display().to_string();
                    }
                }
            });
            ui.add_space(6.0);
            ui.label(egui::RichText::new(match store {
                GameStore::Xbox => "The Xbox package will be downloaded into this folder.",
                GameStore::Epic => "The Epic game files will be installed into this folder.",
            }).size(11.0).color(egui::Color32::GRAY));
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    dismiss = true;
                }
                let valid_directory = Path::new(directory.trim()).is_absolute();
                if ui
                    .add_enabled(valid_directory, egui::Button::new("Download"))
                    .clicked()
                {
                    confirm = true;
                }
            });
        });

    if confirm {
        out.push(Action::ConfirmDownload {
            directory: directory.trim().to_owned(),
        });
    } else if dismiss || !open {
        out.push(Action::DismissDownload);
    }
}
