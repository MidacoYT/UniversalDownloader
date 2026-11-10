//! Carte d'un jeu dans la grille du catalogue.
use super::{Action, icons, theme, widgets};
use crate::images;
use crate::model::{Game, GameStore};
use eframe::egui::{self, Color32, RichText};

pub fn show(
    ui: &mut egui::Ui,
    game: &Game,
    texture: Option<&egui::TextureHandle>,
    loading: bool,
    out: &mut Vec<Action>,
) {
    let hover_id = ui.make_persistent_id(("game_card_hover", &game.id));
    let previous = ui.ctx().data(|d| d.get_temp::<egui::Rect>(hover_id));

    // Zone « ouvrir le détail » enregistrée AVANT le contenu : les boutons restent prioritaires.
    let open = previous.map(|rect| ui.interact(rect, hover_id.with("open"), egui::Sense::click()));
    let hovered = previous.is_some_and(|rect| {
        ui.is_rect_visible(rect)
            && ui.input(|i| i.pointer.hover_pos().is_some_and(|p| rect.contains(p)))
    });
    let hover = ui
        .ctx()
        .animate_bool_with_time(hover_id.with("animation"), hovered, 0.35);
    let zoom = ui
        .ctx()
        .animate_bool_with_time(hover_id.with("zoom"), hovered, 0.5);

    let frame = egui::Frame::new()
        .fill(theme::mix((22, 26, 34), (30, 35, 46), hover))
        .stroke(egui::Stroke::new(
            1.0,
            theme::BORDER.lerp_to_gamma(theme::ACCENT, hover),
        ))
        .shadow(egui::epaint::Shadow {
            offset: [0, 6],
            blur: (20.0 * hover) as u8,
            spread: 0,
            color: Color32::from_black_alpha((120.0 * hover) as u8),
        })
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::same(0))
        .show(ui, |card| {
            card.set_min_width(card.available_width());
            card.set_max_width(card.available_width());
            card.spacing_mut().item_spacing.y = 0.0;
            art(card, game, texture, zoom);
            body(card, game, loading, out);
        });

    ui.ctx()
        .data_mut(|d| d.insert_temp(hover_id, frame.response.rect));
    if open.is_some_and(|r| r.clicked()) {
        out.push(Action::OpenGame {
            id: game.id.clone(),
            store: game.store,
        });
    }
}

fn art(card: &mut egui::Ui, game: &Game, texture: Option<&egui::TextureHandle>, zoom: f32) {
    let width = card.available_width();
    let (rect, _) =
        card.allocate_exact_size(egui::vec2(width, width * 9.0 / 16.0), egui::Sense::hover());
    match texture {
        Some(texture) => images::paint_cover(
            card,
            texture,
            rect,
            egui::CornerRadius {
                nw: 6,
                ne: 6,
                sw: 0,
                se: 0,
            },
            1.0 + 0.08 * zoom,
        ),
        None => images::paint_placeholder(
            card,
            rect,
            egui::CornerRadius {
                nw: 6,
                ne: 6,
                sw: 0,
                se: 0,
            },
            if game.image.is_empty() {
                "AUCUNE IMAGE"
            } else {
                "LOADING…"
            },
        ),
    }

    let tag = match game.store {
        GameStore::Xbox if game.ea_play => "EA PLAY",
        GameStore::Xbox => game
            .genres
            .first()
            .map(String::as_str)
            .unwrap_or("GAME PASS"),
        GameStore::Epic => game
            .genres
            .first()
            .map(String::as_str)
            .unwrap_or("EPIC GAMES"),
    };
    let badge_font = egui::FontId::proportional(11.0);
    let text_width = card
        .painter()
        .layout_no_wrap(tag.to_owned(), badge_font.clone(), Color32::WHITE)
        .size()
        .x;
    let badge = egui::Rect::from_min_size(
        rect.left_bottom() + egui::vec2(8.0, -27.0),
        egui::vec2((text_width + 12.0).max(42.0), 20.0),
    );
    card.painter()
        .rect_filled(badge, 4, Color32::from_rgba_unmultiplied(8, 10, 14, 215));
    card.painter().rect_stroke(
        badge,
        4,
        egui::Stroke::new(1.0, genre_color(tag)),
        egui::StrokeKind::Inside,
    );
    card.painter().text(
        badge.center(),
        egui::Align2::CENTER_CENTER,
        tag,
        badge_font,
        Color32::WHITE,
    );
}

fn genre_color(genre: &str) -> Color32 {
    match genre.to_ascii_lowercase().as_str() {
        "game pass" => theme::GREEN,
        "epic games" => theme::EPIC_BLUE,
        "action" => Color32::from_rgb(249, 115, 22),
        "adventure" => theme::ACCENT_LIGHT,
        "rpg" => theme::GOLD,
        "racing" => theme::GREEN,
        "strategy" => theme::BLUE,
        "simulation" => theme::CYAN,
        _ => theme::TEXT_MUTED,
    }
}

fn body(card: &mut egui::Ui, game: &Game, loading: bool, out: &mut Vec<Action>) {
    let subtitle = if !game.publisher.is_empty() {
        game.publisher.as_str()
    } else if loading {
        "Loading details…"
    } else {
        "Unknown publisher"
    };
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(card, |ui| {
            ui.set_min_width(ui.available_width());
            ui.add_sized(
                [ui.available_width(), 24.0],
                egui::Label::new(
                    RichText::new(game.display_name())
                        .size(15.0)
                        .strong()
                        .color(theme::TEXT_TITLE),
                )
                .truncate(),
            );
            ui.add_sized(
                [ui.available_width(), 19.0],
                egui::Label::new(RichText::new(subtitle).size(12.0).color(theme::TEXT_SUBTLE))
                    .truncate(),
            );
            ui.add_space(7.0);
            meta_row(
                ui,
                vec![
                    (icons::storage(13.0), or_default(&game.size, "— GB")),
                    (
                        icons::players(13.0),
                        or_default(&game.players, "Joueurs : n/d"),
                    ),
                ],
            );
            meta_row(
                ui,
                vec![(icons::shield(13.0), or_default(&game.rating, "PEGI : n/d"))],
            );
            ui.add_space(10.0);
            actions(ui, game, out);
        });
}

fn or_default(value: &str, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value.to_owned()
    }
}

fn meta_row(ui: &mut egui::Ui, items: Vec<(egui::Image<'static>, String)>) {
    // Largeur imposée + texte tronqué : un libellé long (« Single-player · Multiplayer »)
    // ne doit jamais élargir la carte et casser la grille.
    let width = ui.available_width();
    ui.allocate_ui_with_layout(
        egui::vec2(width, 18.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_max_width(width);
            for (image, text) in items {
                ui.add(image);
                ui.add(egui::Label::new(widgets::muted(text, 11.0)).truncate());
            }
        },
    );
}

fn actions(ui: &mut egui::Ui, game: &Game, out: &mut Vec<Action>) {
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), 32.0),
        egui::Layout::left_to_right(egui::Align::Center).with_main_align(egui::Align::Center),
        |ui| {
            if game.store == crate::model::GameStore::Xbox {
                let download = egui::Button::image_and_text(icons::download(15.0), "Download")
                    .fill(theme::ACCENT)
                    .corner_radius(6.0);
                if ui.add(download).clicked() {
                    out.push(Action::PrepareDownload {
                        id: game.id.clone(),
                        name: game.display_name().to_owned(),
                        store: game.store,
                    });
                }
            } else {
                let download = egui::Button::image_and_text(icons::download(15.0), "Download")
                    .fill(theme::ACCENT)
                    .corner_radius(6.0);
                if ui
                    .add_enabled(!game.manifest_url.is_empty(), download)
                    .clicked()
                {
                    out.push(Action::PrepareDownload {
                        id: game.id.clone(),
                        name: game.display_name().to_owned(),
                        store: game.store,
                    });
                }
            }
            if ui
                .add(egui::Button::image(icons::info(16.0)).corner_radius(6.0))
                .on_hover_text("Details")
                .clicked()
            {
                out.push(Action::OpenGame {
                    id: game.id.clone(),
                    store: game.store,
                });
            }
        },
    );
}
