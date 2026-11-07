//! Fenêtre de confirmation de désinstallation.
use super::{Action, theme};
use crate::library::LibraryEntry;
use eframe::egui;

pub fn show(ctx: &egui::Context, entry: &LibraryEntry, out: &mut Vec<Action>) {
    let mut open = true;
    let mut confirm = false;
    let mut dismiss = false;

    egui::Window::new("Uninstall game?")
        .id(egui::Id::new("uninstall_confirmation"))
        .collapsible(false)
        .resizable(false)
        .default_width(460.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label(format!("Uninstall {}?", entry.name));
            ui.add_space(6.0);
            ui.label("This permanently deletes all files in this game folder:");
            ui.add(
                egui::Label::new(entry.path.display().to_string())
                    .wrap_mode(egui::TextWrapMode::Wrap),
            );
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    dismiss = true;
                }
                if ui
                    .add(egui::Button::new("Uninstall").fill(theme::DANGER))
                    .clicked()
                {
                    confirm = true;
                }
            });
        });

    if confirm {
        out.push(Action::ConfirmUninstall);
    } else if !open || dismiss {
        out.push(Action::DismissUninstall);
    }
}
