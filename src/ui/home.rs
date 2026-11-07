//! Page d'accueil : statistiques + carrousel des jeux les plus évalués.
use super::{Action, Page, theme, widgets};
use crate::catalog::Catalog;
use crate::images::{self, ImageCache};
use crate::library::Library;
use crate::model::Game;
use crate::util::{format_count, format_size};
use eframe::egui::{self, Color32, RichText};
use fluent_egui::winui::prelude::*;

const TILE_SIZE: egui::Vec2 = egui::vec2(184.0, 256.0);
const TILE_ART_HEIGHT: f32 = 180.0;

pub fn show(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    epic_catalog: &Catalog,
    library: &Library,
    images: &mut ImageCache,
    out: &mut Vec<Action>,
) {
    widgets::page_header(
        ui,
        "WELCOME BACK",
        "Discover games",
        "Explore popular titles from the Xbox and Epic Games catalogs.",
    );

    if catalog.games.is_empty() && epic_catalog.games.is_empty() {
        empty_state(ui, catalog, epic_catalog, out);
        return;
    }

    ui.winui(|ui| {
        stat_cards(ui, catalog, epic_catalog, library);
        ui.add_space(18.0);
        top_games_header(ui, catalog, epic_catalog, out);
        ui.add_space(8.0);
        top_games_row(ui, catalog, epic_catalog, images, out);
        ui.add_space(20.0);
        new_games_row(ui, catalog, images, out);
    });
}

fn new_games_row(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    images: &mut ImageCache,
    out: &mut Vec<Action>,
) {
    let mut new_games: Vec<_> = catalog
        .games
        .iter()
        .filter(|game| !game.date_added.is_empty())
        .collect();
    new_games.sort_by(|a, b| b.date_added.cmp(&a.date_added));
    new_games.truncate(8);
    if new_games.is_empty() {
        return;
    }
    ui.label(RichText::new("New on Game Pass").size(18.0).strong());
    ui.add_space(8.0);
    egui::ScrollArea::horizontal()
        .id_salt("home_new_games")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                for (rank, game) in new_games.iter().enumerate() {
                    images.request(ui.ctx(), &game.id, &game.image, (480, 480));
                    if tile(ui, rank, game, images.get(&game.id)) {
                        out.push(Action::OpenGame {
                            id: game.id.clone(),
                            store: game.store,
                        });
                    }
                }
            });
        });
}

fn empty_state(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    epic_catalog: &Catalog,
    out: &mut Vec<Action>,
) {
    ui.vertical_centered(|ui| {
        if catalog.is_loading() || epic_catalog.is_loading() {
            ui.spinner();
            ui.label("Loading games…");
        } else {
            ui.label("No games available. Check your connection and refresh the catalog.");
            if ui.button("Open catalog").clicked() {
                out.push(Action::Navigate(Page::Catalog));
            }
        }
    });
}

fn stat_cards(ui: &mut egui::Ui, catalog: &Catalog, epic_catalog: &Catalog, library: &Library) {
    let stats = [
        (
            "Available games",
            (catalog.games.len() + epic_catalog.games.len()).to_string(),
            theme::ACCENT_LIGHT,
        ),
        (
            "Installed games",
            library.entries.len().to_string(),
            theme::GREEN,
        ),
        (
            "Storage used",
            format_size(library.total_bytes()),
            theme::BLUE,
        ),
    ];
    ui.horizontal(|ui| {
        for (label, value, color) in stats {
            egui::Frame::new()
                .fill(theme::BG_CARD)
                .stroke(egui::Stroke::new(
                    1.0,
                    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 55),
                ))
                .corner_radius(10)
                .inner_margin(egui::Margin::symmetric(14, 10))
                .show(ui, |card| {
                    card.label(RichText::new(value).size(19.0).strong().color(color));
                    card.label(widgets::muted(label, 10.0).color(Color32::GRAY));
                });
        }
    });
}

fn top_games_header(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    epic_catalog: &Catalog,
    out: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Top Games").size(18.0).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("View full catalog →").clicked() {
                out.push(Action::Navigate(Page::Catalog));
            }
            ui.label(
                RichText::new(format!(
                    "{} games · Xbox most reviewed / Epic recent releases",
                    catalog.games.len() + epic_catalog.games.len()
                ))
                .size(11.0)
                .color(Color32::GRAY),
            );
        });
    });
}

fn top_games_row(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    epic_catalog: &Catalog,
    images: &mut ImageCache,
    out: &mut Vec<Action>,
) {
    // Xbox is ordered by review count; Epic has no public player/review totals in
    // the manifest feed, so show its newest known releases without implying a rank.
    let xbox = catalog.top_games(5);
    let mut epic: Vec<_> = epic_catalog.games.iter().collect();
    epic.sort_by(|a, b| b.release.cmp(&a.release).then_with(|| a.name.cmp(&b.name)));
    epic.truncate(5);
    let mut top = Vec::new();
    for index in 0..5 {
        if let Some(game) = xbox.get(index) {
            top.push((*game, index));
        }
        if let Some(game) = epic.get(index) {
            top.push((*game, index));
        }
    }
    egui::ScrollArea::horizontal()
        .id_salt("home_top_games")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                for (game, rank) in top.iter() {
                    images.request(ui.ctx(), &game.id, &game.image, (480, 480));
                    if tile(ui, *rank, game, images.get(&game.id)) {
                        out.push(Action::OpenGame {
                            id: game.id.clone(),
                            store: game.store,
                        });
                    }
                }
            });
        });
}

/// Carte portrait d'un jeu ; retourne `true` au clic.
fn tile(
    ui: &mut egui::Ui,
    rank: usize,
    game: &Game,
    texture: Option<&egui::TextureHandle>,
) -> bool {
    let (rect, response) = ui.allocate_exact_size(TILE_SIZE, egui::Sense::click());
    let hover = ui.ctx().animate_bool_with_time(
        response.id.with("home-card-hover"),
        response.hovered(),
        0.18,
    );
    let painter = ui.painter();
    painter.rect_filled(rect, 12.0, theme::mix((22, 20, 36), (28, 25, 48), hover));

    let art = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), TILE_ART_HEIGHT));
    match texture {
        Some(texture) => images::paint_cover(ui, texture, art, images::top_radius(12), 1.0),
        None => {
            painter.text(
                art.center(),
                egui::Align2::CENTER_CENTER,
                &game.name,
                egui::FontId::proportional(13.0),
                theme::TEXT_MUTED,
            );
        }
    }

    let (store_label, store_color) = match game.store {
        crate::model::GameStore::Xbox => ("XBOX", theme::GREEN),
        crate::model::GameStore::Epic => ("EPIC", theme::ACCENT),
    };
    let badge = egui::Rect::from_min_size(rect.min + egui::vec2(8.0, 8.0), egui::vec2(58.0, 22.0));
    painter.rect_filled(badge, 6.0, Color32::from_rgba_unmultiplied(10, 10, 18, 210));
    painter.text(
        badge.center(),
        egui::Align2::CENTER_CENTER,
        format!("{store_label} #{}", rank + 1),
        egui::FontId::proportional(9.0),
        store_color,
    );

    let text_width = rect.width() - 20.0;
    widgets::paint_truncated(
        painter,
        rect.min + egui::vec2(10.0, 190.0),
        &game.name,
        egui::FontId::proportional(13.0),
        theme::TEXT_TITLE,
        text_width,
    );
    let subtitle = if let Some(score) = game.critic_score {
        format!("OpenCritic {score}/100")
    } else if game.rating_count > 0 {
        format!("{} reviews", format_count(game.rating_count))
    } else {
        game.publisher.clone()
    };
    widgets::paint_truncated(
        painter,
        rect.min + egui::vec2(10.0, 232.0),
        &subtitle,
        egui::FontId::proportional(10.0),
        theme::TEXT_SUBTLE,
        text_width,
    );

    let clicked = response.clicked();
    response.on_hover_cursor(egui::CursorIcon::PointingHand);
    clicked
}
