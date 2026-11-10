//! Barre supérieure « Connect » : marque à gauche, navigation au centre, téléchargement,
//! paramètres et boutons de fenêtre à droite. Elle remplace la barre de titre native
//! (la fenêtre n'a plus de décorations) : tout son fond sert aussi à déplacer la fenêtre.
use super::{Action, Page, icons, theme, window};
use crate::ui::sidebar::DownloadSummary;
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, StrokeKind, vec2};

pub const HEIGHT: f32 = 64.0;

const PADDING: f32 = 32.0;
const NAV_GAP: f32 = 48.0;
const NAV_ITEM_PADDING: f32 = 16.0;
const SETTINGS_SIZE: f32 = 36.0;
const DOWNLOAD_WIDTH: f32 = 230.0;
const DOWNLOAD_HEIGHT: f32 = 36.0;

const NAV: [(Page, &str); 5] = [
    (Page::Home, "Home"),
    (Page::Catalog, "Catalog"),
    (Page::Library, "Library"),
    (Page::Downloads, "Downloads"),
    (Page::Updates, "Updates"),
];

pub fn show(
    ui: &mut egui::Ui,
    current: Page,
    download: Option<&DownloadSummary>,
    out: &mut Vec<Action>,
) {
    let bar = ui.max_rect();

    paint_background(ui, bar);
    // Enregistré en premier : tous les widgets ci-dessous passent au-dessus.
    window::drag_region(ui, bar);

    let brand_right = brand(ui, bar);
    navigation(ui, bar, brand_right + NAV_GAP, current, out);

    // De droite à gauche : boutons de fenêtre, paramètres, téléchargement en cours.
    let mut right = window::caption_buttons(ui, bar) - 12.0;
    right = settings_button(ui, bar, right, current, out) - 12.0;
    if let Some(download) = download {
        download_pill(ui, bar, right, download, out);
    }
}

/// Animation de survol partagée par tous les boutons de la barre.
fn hover_amount(ui: &egui::Ui, response: &egui::Response) -> f32 {
    ui.ctx()
        .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.12)
}

fn paint_background(ui: &egui::Ui, bar: Rect) {
    let painter = ui.painter();
    painter.rect_filled(bar, 0.0, theme::BG_SIDEBAR);
    painter.hline(
        bar.x_range(),
        bar.bottom() - 0.5,
        egui::Stroke::new(1.0, theme::BORDER),
    );
}

/// Logo + nom. Retourne le bord droit du texte.
fn brand(ui: &egui::Ui, bar: Rect) -> f32 {
    let painter = ui.painter();
    let mark = Rect::from_min_size(
        Pos2::new(bar.left() + PADDING, bar.center().y - 14.0),
        vec2(28.0, 28.0),
    );
    painter.rect_filled(mark, 6.0, theme::ACCENT);
    painter.text(
        mark.center(),
        egui::Align2::CENTER_CENTER,
        "U",
        FontId::proportional(15.0),
        Color32::WHITE,
    );
    painter
        .text(
            Pos2::new(mark.right() + 10.0, bar.center().y),
            egui::Align2::LEFT_CENTER,
            "UNIVERSAL DOWNLOADER",
            FontId::proportional(12.0),
            theme::TEXT_TITLE,
        )
        .right()
}

fn navigation(ui: &mut egui::Ui, bar: Rect, mut x: f32, current: Page, out: &mut Vec<Action>) {
    for (index, (page, label)) in NAV.into_iter().enumerate() {
        let galley = ui.painter().layout_no_wrap(
            label.to_owned(),
            FontId::proportional(13.0),
            theme::TEXT_MUTED,
        );
        let width = galley.size().x + NAV_ITEM_PADDING * 2.0;
        let item = Rect::from_min_size(Pos2::new(x, bar.top()), vec2(width, bar.height()));

        let response = ui.interact(item, ui.id().with(("topnav", index)), Sense::click());
        let hover = hover_amount(ui, &response);
        let active = ui
            .ctx()
            .animate_bool_with_time(response.id.with("active"), current == page, 0.2);
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        let painter = ui.painter();
        painter.galley(
            Pos2::new(x + NAV_ITEM_PADDING, item.center().y - galley.size().y / 2.0),
            galley,
            theme::mix((138, 146, 160), (245, 247, 250), hover.max(active)),
        );
        if active > 0.0 {
            let underline = Rect::from_min_max(
                Pos2::new(x + 8.0, bar.bottom() - 3.0),
                Pos2::new(x + width - 8.0, bar.bottom()),
            );
            painter.rect_filled(underline, 1.0, theme::ACCENT.gamma_multiply(active));
        }
        if response.clicked() {
            out.push(Action::Navigate(page));
        }
        x += width;
    }
}

/// Bouton paramètres aligné à `right`. Retourne son bord gauche.
fn settings_button(
    ui: &mut egui::Ui,
    bar: Rect,
    right: f32,
    current: Page,
    out: &mut Vec<Action>,
) -> f32 {
    let rect = Rect::from_min_size(
        Pos2::new(right - SETTINGS_SIZE, bar.center().y - SETTINGS_SIZE / 2.0),
        vec2(SETTINGS_SIZE, SETTINGS_SIZE),
    );
    let response = ui.interact(rect, ui.id().with("topbar-settings"), Sense::click());
    let hover = hover_amount(ui, &response);

    ui.painter()
        .rect_filled(rect, 6.0, theme::mix((22, 26, 34), (30, 35, 46), hover));
    let tint = if current == Page::Settings {
        theme::ACCENT_LIGHT
    } else {
        theme::TEXT_MUTED
    };
    icons::settings(16.0)
        .tint(tint)
        .paint_at(ui, Rect::from_center_size(rect.center(), vec2(16.0, 16.0)));

    if response.clicked() {
        out.push(Action::Navigate(Page::Settings));
    }
    rect.left()
}

/// Pastille de progression du téléchargement en cours, alignée à `right`.
fn download_pill(
    ui: &mut egui::Ui,
    bar: Rect,
    right: f32,
    download: &DownloadSummary,
    out: &mut Vec<Action>,
) {
    let pill = Rect::from_min_size(
        Pos2::new(right - DOWNLOAD_WIDTH, bar.center().y - DOWNLOAD_HEIGHT / 2.0),
        vec2(DOWNLOAD_WIDTH, DOWNLOAD_HEIGHT),
    );
    let response = ui.interact(pill, ui.id().with("topbar-download"), Sense::click());
    let hover = hover_amount(ui, &response);

    let painter = ui.painter();
    painter.rect_filled(pill, 6.0, theme::mix((22, 26, 34), (30, 35, 46), hover));
    painter.rect_stroke(
        pill,
        6.0,
        egui::Stroke::new(1.0, theme::BORDER),
        StrokeKind::Inside,
    );

    let percent = download
        .progress
        .map(|p| format!("{:.0} %", p * 100.0))
        .unwrap_or_default();
    let text_y = pill.center().y - 4.0;
    painter.text(
        Pos2::new(pill.left() + 12.0, text_y),
        egui::Align2::LEFT_CENTER,
        download.label,
        FontId::proportional(11.5),
        theme::TEXT_TITLE,
    );
    painter.text(
        Pos2::new(pill.right() - 12.0, text_y),
        egui::Align2::RIGHT_CENTER,
        percent,
        FontId::proportional(11.5),
        theme::TEXT_MUTED,
    );

    let track_y = pill.bottom() - 4.0;
    let (track_left, track_right) = (pill.left() + 10.0, pill.right() - 10.0);
    painter.line_segment(
        [Pos2::new(track_left, track_y), Pos2::new(track_right, track_y)],
        egui::Stroke::new(2.0, theme::BORDER),
    );
    if let Some(progress) = download.progress {
        let end = track_left + (track_right - track_left) * progress;
        let color = if download.paused { theme::GOLD } else { theme::ACCENT };
        painter.line_segment(
            [Pos2::new(track_left, track_y), Pos2::new(end, track_y)],
            egui::Stroke::new(2.0, color),
        );
    }

    if response.clicked() {
        out.push(Action::Navigate(Page::Downloads));
    }
    response.on_hover_text(download.name.as_str());
}
