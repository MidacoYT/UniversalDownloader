//! Widgets partagés : boutons, listes déroulantes et en-têtes au style « Connect ».
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

/// En-tête de page : kicker en majuscules espacé, grand titre, sous-titre discret.
pub fn page_header(ui: &mut egui::Ui, kicker: &str, title: &str, subtitle: &str) {
    ui.add_space(26.0);
    ui.label(
        RichText::new(kicker.to_uppercase())
            .size(11.0)
            .strong()
            .color(theme::ACCENT_LIGHT),
    );
    ui.add_space(2.0);
    ui.heading(RichText::new(title).size(30.0).strong().color(theme::TEXT_TITLE));
    ui.label(RichText::new(subtitle).size(13.0).color(theme::TEXT_MUTED));
    ui.add_space(14.0);
}

/// Ligne de statut discrète, précédée d'un spinner si `busy`.
pub fn status_line(ui: &mut egui::Ui, busy: bool, text: &str) {
    ui.horizontal(|ui| {
        if busy {
            ui.spinner();
        }
        ui.label(RichText::new(text).size(11.0).color(theme::TEXT_SUBTLE));
    });
}

/// Hauteur commune des boutons / listes (filtres, pagination, en-têtes).
pub const PILL_HEIGHT: f32 = 34.0;
const PILL_RADIUS: u8 = 6;

fn pill_state(w: &mut egui::style::WidgetVisuals, fill: Color32, stroke: Color32) {
    w.bg_fill = fill;
    w.weak_bg_fill = fill;
    w.bg_stroke = egui::Stroke::new(1.0, stroke);
    w.fg_stroke = egui::Stroke::new(1.0, theme::TEXT_TITLE);
    w.corner_radius = egui::CornerRadius::same(PILL_RADIUS);
    w.expansion = 0.0;
}

/// Exécute `add` avec le style « bouton sobre » (fond charbon, bordure fine, survol bleu).
/// Les `ComboBox` ajoutés dans la closure héritent de ce style.
pub fn pill_scope<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = PILL_HEIGHT;
        ui.spacing_mut().button_padding = egui::vec2(14.0, 6.0);
        let widgets = &mut ui.visuals_mut().widgets;
        pill_state(&mut widgets.inactive, theme::BG_CARD, theme::BORDER);
        pill_state(&mut widgets.hovered, theme::BG_CARD_HOVER, theme::ACCENT);
        pill_state(&mut widgets.active, theme::ACCENT_DEEP, theme::ACCENT);
        pill_state(&mut widgets.open, theme::BG_CARD_HOVER, theme::ACCENT);
        add(ui)
    })
    .inner
}

/// Liste déroulante générique sur un type `Copy`, au style « Connect ».
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

/// Bouton peint à la main (rendu identique partout). `leading` / `trailing` : icônes optionnelles.
/// Le bouton principal se distingue par un fond bleu plein.
pub fn pill_button(
    ui: &mut egui::Ui,
    text: &str,
    leading: Option<egui::Image<'static>>,
    trailing: Option<egui::Image<'static>>,
    enabled: bool,
) -> egui::Response {
    const PAD: f32 = 16.0;
    const ICON: f32 = 14.0;
    const GAP: f32 = 8.0;
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(12.5),
        Color32::PLACEHOLDER,
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
    let hover_on = enabled && response.hovered();
    let hover = ui
        .ctx()
        .animate_bool_with_time(response.id.with("pill-hover"), hover_on, 0.12);
    if hover_on {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let pressed = enabled && response.is_pointer_button_down_on();
    let fill = if !enabled {
        Color32::from_rgb(20, 23, 30)
    } else if pressed {
        theme::ACCENT_DEEP
    } else {
        theme::mix((30, 35, 46), (42, 49, 64), hover)
    };
    let stroke = if enabled {
        theme::BORDER.lerp_to_gamma(theme::ACCENT, hover)
    } else {
        theme::BORDER
    };
    let painter = ui.painter();
    painter.rect(
        rect,
        egui::CornerRadius::same(PILL_RADIUS),
        fill,
        egui::Stroke::new(1.0, stroke),
        egui::StrokeKind::Inside,
    );
    let color = if enabled {
        theme::TEXT_TITLE
    } else {
        Color32::from_gray(96)
    };
    let mut x = rect.left() + PAD;
    let icon_rect = |x: f32| {
        egui::Rect::from_min_size(
            egui::pos2(x, rect.center().y - ICON / 2.0),
            egui::vec2(ICON, ICON),
        )
    };
    let tint = if enabled {
        Color32::WHITE
    } else {
        Color32::from_gray(96)
    };
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
