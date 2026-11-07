//! Petits widgets et helpers réutilisés par plusieurs pages.
use super::theme;
use eframe::egui::{self, Color32, RichText};

pub fn icon(name: &'static str, bytes: &'static [u8], size: f32) -> egui::Image<'static> {
    egui::Image::from_bytes(name, bytes).fit_to_exact_size(egui::vec2(size, size))
}

pub fn muted(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text.into())
        .size(size)
        .color(theme::TEXT_MUTED)
}

pub fn page_header(ui: &mut egui::Ui, kicker: &str, title: &str, subtitle: &str) {
    ui.add_space(22.0);
    ui.label(
        RichText::new(kicker)
            .size(10.0)
            .strong()
            .color(theme::ACCENT),
    );
    ui.heading(RichText::new(title).size(27.0).strong());
    ui.label(RichText::new(subtitle).color(Color32::GRAY));
    ui.add_space(10.0);
}

/// Ligne de statut grisée, précédée d'un spinner si `busy`.
pub fn status_line(ui: &mut egui::Ui, busy: bool, text: &str) {
    ui.horizontal(|ui| {
        if busy {
            ui.spinner();
        }
        ui.label(RichText::new(text).size(11.0).color(Color32::GRAY));
    });
}

/// Liste déroulante générique sur un type `Copy`.
pub fn combo<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    id: &str,
    current: &mut T,
    options: &[T],
    label: impl Fn(T) -> &'static str,
) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(label(*current))
        .show_ui(ui, |ui| {
            for &option in options {
                ui.selectable_value(current, option, label(option));
            }
        });
}

pub fn identity(value: &'static str) -> &'static str {
    value
}

/// Texte sur une seule ligne, tronqué avec « … », peint sans modifier le layout.
pub fn paint_truncated(
    painter: &egui::Painter,
    pos: egui::Pos2,
    text: &str,
    font: egui::FontId,
    color: Color32,
    width: f32,
) {
    let mut job = egui::text::LayoutJob::simple(text.to_owned(), font, color, width);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let galley = painter.layout_job(job);
    painter.galley(pos, galley, color);
}
