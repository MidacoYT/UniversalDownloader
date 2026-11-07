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

/// Hauteur commune des boutons / listes « pilule » (filtres, pagination, en-têtes).
pub const PILL_HEIGHT: f32 = 32.0;
const PILL_RADIUS: u8 = 10;

fn pill_state(w: &mut egui::style::WidgetVisuals, fill: Color32, stroke: Color32) {
    w.bg_fill = fill;
    w.weak_bg_fill = fill;
    w.bg_stroke = egui::Stroke::new(1.0, stroke);
    w.fg_stroke = egui::Stroke::new(1.0, theme::TEXT_TITLE);
    w.corner_radius = egui::CornerRadius::same(PILL_RADIUS);
    w.expansion = 0.0;
}

/// Exécute `add` avec le style « pilule » (fond violet sombre, bordure accent, survol marqué).
/// Les `ComboBox` ajoutés dans la closure héritent de ce style.
pub fn pill_scope<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = PILL_HEIGHT;
        ui.spacing_mut().button_padding = egui::vec2(12.0, 6.0);
        let widgets = &mut ui.visuals_mut().widgets;
        pill_state(&mut widgets.inactive, Color32::from_rgb(28, 23, 48), theme::accent_alpha(95));
        pill_state(&mut widgets.hovered, Color32::from_rgb(48, 32, 90), theme::ACCENT_LIGHT);
        pill_state(&mut widgets.active, Color32::from_rgb(88, 46, 176), theme::ACCENT_LIGHT);
        pill_state(&mut widgets.open, Color32::from_rgb(48, 32, 90), theme::ACCENT_LIGHT);
        add(ui)
    })
    .inner
}

/// Liste déroulante générique sur un type `Copy`, au style « pilule ».
pub fn combo<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    id: &str,
    current: &mut T,
    options: &[T],
    label: impl Fn(T) -> &'static str,
) {
    pill_scope(ui, |ui| {
        egui::ComboBox::from_id_salt(id)
            .selected_text(label(*current))
            .show_ui(ui, |ui| {
                for &option in options {
                    ui.selectable_value(current, option, label(option));
                }
            });
    });
}

/// Bouton « pilule » peint à la main (rendu identique partout, indépendant du thème Fluent).
/// `leading` / `trailing` : icônes optionnelles avant / après le texte.
pub fn pill_button(
    ui: &mut egui::Ui,
    text: &str,
    leading: Option<egui::Image<'static>>,
    trailing: Option<egui::Image<'static>>,
    enabled: bool,
) -> egui::Response {
    const PAD: f32 = 14.0;
    const ICON: f32 = 14.0;
    const GAP: f32 = 7.0;
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(12.0),
        Color32::PLACEHOLDER, // remplacée par la couleur passée à `galley` ci-dessous
    );
    let text_width = galley.size().x;
    let icon_count = leading.is_some() as u8 + trailing.is_some() as u8;
    let width = PAD * 2.0 + text_width + f32::from(icon_count) * (ICON + GAP);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, PILL_HEIGHT),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let active = enabled && response.hovered();
    let hover = ui
        .ctx()
        .animate_bool_with_time(response.id.with("pill-hover"), active, 0.14);
    if active {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let pressed = enabled && response.is_pointer_button_down_on();
    let fill = if pressed {
        theme::mix((48, 32, 90), (88, 46, 176), 1.0)
    } else {
        theme::mix((28, 23, 48), (48, 32, 90), hover)
    };
    let stroke = if enabled {
        theme::accent_alpha(95).lerp_to_gamma(theme::ACCENT_LIGHT, hover)
    } else {
        Color32::from_rgb(45, 42, 62)
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        egui::CornerRadius::same(PILL_RADIUS),
        if enabled { fill } else { Color32::from_rgb(22, 21, 33) },
        egui::Stroke::new(1.0, stroke),
        egui::StrokeKind::Inside,
    );
    let color = if enabled {
        theme::TEXT_TITLE
    } else {
        Color32::from_gray(105)
    };
    let mut x = rect.left() + PAD;
    let icon_rect = |x: f32| {
        egui::Rect::from_min_size(
            egui::pos2(x, rect.center().y - ICON / 2.0),
            egui::vec2(ICON, ICON),
        )
    };
    let tint = if enabled { Color32::WHITE } else { Color32::from_gray(105) };
    if let Some(image) = leading {
        image.tint(tint).paint_at(ui, icon_rect(x));
        x += ICON + GAP;
    }
    let text_pos = egui::pos2(x, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(text_pos, galley, color);
    x += text_width + GAP;
    if let Some(image) = trailing {
        image.tint(tint).paint_at(ui, icon_rect(x));
    }
    response
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
