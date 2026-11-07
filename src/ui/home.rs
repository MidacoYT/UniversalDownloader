//! Page d'accueil : statistiques + carrousel des jeux les plus évalués.
use super::{Action, Page, icons, theme, widgets};
use crate::catalog::Catalog;
use crate::images::{self, ImageCache};
use crate::library::Library;
use crate::model::Game;
use crate::util::{format_count, format_size, top_k};
use eframe::egui::{self, Color32, RichText};
use fluent_egui::winui::prelude::*;

const TILE_SIZE: egui::Vec2 = egui::vec2(184.0, 256.0);
const TILE_ART_HEIGHT: f32 = 180.0;
/// Diamètre des flèches du carrousel et marge entre elles et le bord du carrousel.
const ARROW_SIZE: f32 = 40.0;
const ARROW_INSET: f32 = 10.0;

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
    let new_games = top_k(
        catalog
            .games
            .iter()
            .filter(|game| !game.date_added.is_empty()),
        8,
        |a, b| b.date_added.cmp(&a.date_added),
    );
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
            if widgets::pill_button(
                ui,
                "View full catalog",
                None,
                Some(icons::chevron_right(14.0)),
                true,
            )
            .clicked()
            {
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
    let epic = top_k(epic_catalog.games.iter(), 5, |a, b| {
        b.release_date_sort
            .cmp(&a.release_date_sort)
            .then_with(|| a.name.cmp(&b.name))
    });
    let mut top = Vec::with_capacity(10);
    for index in 0..5 {
        if let Some(game) = xbox.get(index) {
            top.push((*game, index));
        }
        if let Some(game) = epic.get(index) {
            top.push((*game, index));
        }
    }
    let offset_id = egui::Id::new("home_top_games_arrow_target");
    let current_offset_id = egui::Id::new("home_top_games_arrow_current");
    let animation_id = egui::Id::new("home_top_games_arrow_animation");
    let current_offset = ui
        .ctx()
        .data(|data| data.get_temp::<f32>(current_offset_id))
        .unwrap_or(0.0);
    let target_offset = ui
        .ctx()
        .data(|data| data.get_temp::<f32>(offset_id))
        .unwrap_or(current_offset);
    let animated_offset = ui
        .ctx()
        .animate_value_with_time(animation_id, target_offset, 0.28);
    let sliding = (animated_offset - target_offset).abs() > 0.5;

    // Le carrousel occupe toute la largeur ; les flèches sont dessinées PAR-DESSUS, sur ses
    // bords gauche/droit, centrées verticalement sur la hauteur des cartes.
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), TILE_SIZE.y),
        egui::Sense::hover(),
    );
    let step = (TILE_SIZE.x + ui.spacing().item_spacing.x) * 2.0;
    let mut area = egui::ScrollArea::horizontal()
        .id_salt("home_top_games")
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden);
    if sliding {
        area = area.horizontal_scroll_offset(animated_offset);
    }
    let output = ui
        .scope_builder(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::left_to_right(egui::Align::Min)),
            |ui| {
                area.show(ui, |ui| {
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
                })
            },
        )
        .inner;
    let offset = output.state.offset.x;
    ui.ctx()
        .data_mut(|data| data.insert_temp(current_offset_id, offset));
    if sliding {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(16));
    }
    let max_offset = (output.content_size.x - output.inner_rect.width()).max(0.0);

    // Dessinées après le ScrollArea : au-dessus des cartes, et prioritaires pour le clic.
    let center_y = rect.center().y;
    let half = ARROW_SIZE / 2.0;
    let left = carousel_arrow(
        ui,
        egui::pos2(rect.left() + ARROW_INSET + half, center_y),
        false,
        offset > 0.5,
    );
    let right = carousel_arrow(
        ui,
        egui::pos2(rect.right() - ARROW_INSET - half, center_y),
        true,
        offset < max_offset - 0.5,
    );
    if left || right {
        let base = target_offset.max(offset);
        let next = if left {
            (base - step).max(0.0)
        } else {
            (base + step).min(max_offset)
        };
        ui.ctx().data_mut(|data| data.insert_temp(offset_id, next));
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(16));
    } else if !sliding {
        ui.ctx().data_mut(|data| data.remove_temp::<f32>(offset_id));
    }
}

/// Flèche ronde posée sur le carrousel, centrée sur `center`. Retourne `true` au clic.
/// Elle s'efface (et ne capte plus la souris) quand il n'y a plus rien à faire défiler.
fn carousel_arrow(ui: &mut egui::Ui, center: egui::Pos2, right: bool, enabled: bool) -> bool {
    let id = ui.make_persistent_id(("home_top_games_arrow", right));
    let visible = ui
        .ctx()
        .animate_bool_with_time(id.with("visible"), enabled, 0.15);
    if visible <= 0.0 {
        return false;
    }
    let rect = egui::Rect::from_center_size(center, egui::vec2(ARROW_SIZE, ARROW_SIZE));
    let response = ui.interact(
        rect,
        id,
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let active = enabled && response.hovered();
    if active {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let hover = ui
        .ctx()
        .animate_bool_with_time(id.with("hover"), active, 0.14);

    let radius = ARROW_SIZE / 2.0;
    let painter = ui.painter();
    painter.circle_filled(
        center + egui::vec2(0.0, 2.0),
        radius + 2.0,
        Color32::from_black_alpha(90).gamma_multiply(visible),
    );
    painter.circle(
        center,
        radius,
        theme::mix((32, 27, 48), (119, 62, 220), hover).gamma_multiply(0.95 * visible),
        egui::Stroke::new(1.0, theme::accent_alpha(150).gamma_multiply(visible)),
    );
    // Chevron tracé en vectoriel : toujours centré, contrairement à un glyphe « ‹ » / « › »
    // dont la position verticale dépend de la police.
    let dir = if right { 1.0 } else { -1.0 };
    let (dx, dy) = (3.5 * dir, 7.0);
    painter.add(egui::Shape::line(
        vec![
            egui::pos2(center.x - dx, center.y - dy),
            egui::pos2(center.x + dx, center.y),
            egui::pos2(center.x - dx, center.y + dy),
        ],
        egui::Stroke::new(2.5, Color32::WHITE.gamma_multiply(visible)),
    ));
    enabled && response.clicked()
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
