//! Fenêtre de confirmation de désinstallation.
use super::{Action, theme};
use crate::library::{LibraryEntry, UninstallProgress};
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
            ui.label("The game will be removed from Windows if needed, then its files will be permanently deleted:");
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

/// Fenêtre d'avancement pendant la désinstallation (barre fine + étape en cours).
pub fn show_progress(ctx: &egui::Context, progress: &UninstallProgress) {
    egui::Window::new("Uninstalling…")
        .id(egui::Id::new("uninstall_progress"))
        .collapsible(false)
        .resizable(false)
        .title_bar(false)
        .default_width(420.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.set_min_width(380.0);
            ui.label(
                egui::RichText::new(format!("Uninstalling {}", progress.name))
                    .size(16.0)
                    .strong(),
            );
            ui.add_space(4.0);
            ui.label(super::widgets::muted(&progress.stage, 12.0));
            ui.add_space(10.0);
            match progress.fraction {
                Some(fraction) => {
                    let fraction = fraction.clamp(0.0, 1.0);
                    ui.horizontal(|row| {
                        let percent_width = 42.0;
                        let bar_width =
                            (row.available_width() - percent_width - row.spacing().item_spacing.x)
                                .max(24.0);
                        row.add(
                            egui::ProgressBar::new(fraction)
                                .desired_width(bar_width)
                                .desired_height(4.0)
                                .corner_radius(2)
                                .fill(theme::DANGER),
                        );
                        row.add_sized(
                            [percent_width, 18.0],
                            egui::Label::new(super::widgets::muted(
                                format!("{:.0}%", fraction * 100.0),
                                11.0,
                            ))
                            .halign(egui::Align::RIGHT),
                        );
                    });
                }
                None => {
                    ui.add(
                        egui::ProgressBar::new(0.0)
                            .desired_width(ui.available_width())
                            .desired_height(4.0)
                            .corner_radius(2)
                            .animate(true),
                    );
                }
            }
        });
}
