//! Surcouche « lightbox » du lecteur de trailers : fond sombre, barre de titre, bouton fermer.
//! La vidéo elle-même est une WebView native positionnée sur `video_rect`.
use super::{Action, theme};
use crate::player::TrailerPlayer;
use eframe::egui::{self, Color32, RichText};

const TITLE_BAR: f32 = 52.0;

pub fn show(
    ctx: &egui::Context,
    frame: &eframe::Frame,
    player: &mut TrailerPlayer,
    out: &mut Vec<Action>,
) {
    if !player.is_open() {
        return;
    }
    let viewport = ctx.input(|i| i.viewport_rect());
    let max_w = (viewport.width() - 96.0).clamp(320.0, 1000.0);
    let max_h = (viewport.height() - 96.0 - TITLE_BAR).max(180.0);
    let (w, h) = if max_w * 9.0 / 16.0 <= max_h {
        (max_w, max_w * 9.0 / 16.0)
    } else {
        (max_h * 16.0 / 9.0, max_h)
    };
    let panel = egui::Rect::from_center_size(viewport.center(), egui::vec2(w, h + TITLE_BAR));
    let video_rect =
        egui::Rect::from_min_size(panel.min + egui::vec2(0.0, TITLE_BAR), egui::vec2(w, h));

    let mut close = ctx.input(|i| i.key_pressed(egui::Key::Escape));

    // Fond : absorbe les clics destinés à l'interface en dessous.
    egui::Area::new(egui::Id::new("trailer_backdrop"))
        .order(egui::Order::Tooltip)
        .fixed_pos(viewport.min)
        .show(ctx, |ui| {
            let (rect, response) = ui.allocate_exact_size(viewport.size(), egui::Sense::click());
            ui.painter()
                .rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 230));
            if response.clicked() {
                close = true;
            }
        });

    // Barre de titre + zone vidéo.
    egui::Area::new(egui::Id::new("trailer_panel"))
        .order(egui::Order::Tooltip)
        .fixed_pos(panel.min)
        .show(ctx, |ui| {
            ui.set_max_width(w);
            let (bar, _) = ui.allocate_exact_size(egui::vec2(w, TITLE_BAR), egui::Sense::hover());
            ui.painter().rect_filled(
                bar,
                egui::CornerRadius {
                    nw: 12,
                    ne: 12,
                    sw: 0,
                    se: 0,
                },
                theme::BG_MODAL,
            );
            let mut title = egui::text::LayoutJob::simple(
                player.title.clone(),
                egui::FontId::proportional(15.0),
                theme::TEXT_TITLE,
                w - 80.0,
            );
            title.wrap.max_rows = 1;
            title.wrap.break_anywhere = true;
            let galley = ui.painter().layout_job(title);
            let pos = egui::pos2(bar.left() + 18.0, bar.center().y - galley.size().y / 2.0);
            ui.painter().galley(pos, galley, theme::TEXT_TITLE);

            let close_rect = egui::Rect::from_center_size(
                egui::pos2(bar.right() - 28.0, bar.center().y),
                egui::vec2(32.0, 32.0),
            );
            let response = ui.interact(
                close_rect,
                ui.id().with("trailer_close"),
                egui::Sense::click(),
            );
            ui.painter().circle_filled(
                close_rect.center(),
                16.0,
                if response.hovered() {
                    Color32::from_rgba_unmultiplied(255, 255, 255, 60)
                } else {
                    Color32::from_rgba_unmultiplied(255, 255, 255, 25)
                },
            );
            ui.painter().text(
                close_rect.center(),
                egui::Align2::CENTER_CENTER,
                "×",
                egui::FontId::proportional(18.0),
                Color32::WHITE,
            );
            if response.clicked() {
                close = true;
            }

            // Emplacement de la vidéo (la WebView native vient se poser dessus).
            let (area, _) = ui.allocate_exact_size(video_rect.size(), egui::Sense::hover());
            ui.painter()
                .rect_filled(area, 0.0, Color32::from_rgb(10, 10, 18));
            if let Some(error) = &player.error {
                ui.scope_builder(egui::UiBuilder::new().max_rect(area.shrink(24.0)), |ui| {
                    ui.label(RichText::new(error).color(theme::TEXT_MUTED));
                    if let Some(url) = player.url() {
                        if ui.button("Open in browser").clicked() {
                            let _ = webbrowser::open(url);
                        }
                    }
                });
            }
        });

    player.sync(frame, ctx.zoom_factor(), video_rect);

    if close {
        out.push(Action::CloseTrailer);
    }
}
