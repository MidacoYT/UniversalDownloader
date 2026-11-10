//! Accueil « Connect » : bannière à la une qui défile, puis rangées horizontales de jeux.
use super::{Action, Page, theme, widgets};
use crate::catalog::Catalog;
use crate::images::{self, ImageCache};
use crate::library::Library;
use crate::model::{Game, GameStore};
use crate::util::{format_size, top_k};
use eframe::egui::epaint::{Mesh, Vertex, WHITE_UV};
use eframe::egui::{self, Color32, RichText};
use std::time::Duration;

const HERO_HEIGHT: f32 = 360.0;
const HERO_ROTATE_SECS: f64 = 7.0;
const CARD_ART_HEIGHT: f32 = 140.0;
const CARD_SIZE: egui::Vec2 = egui::vec2(248.0, CARD_ART_HEIGHT + 58.0);
const RAIL_GAP: f32 = 14.0;

pub fn show(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    epic_catalog: &Catalog,
    library: &Library,
    images: &mut ImageCache,
    out: &mut Vec<Action>,
) {
    egui::ScrollArea::vertical()
        .id_salt("home_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if catalog.games.is_empty() && epic_catalog.games.is_empty() {
                empty_state(ui, catalog, epic_catalog, out);
                return;
            }
            hero(ui, catalog, epic_catalog, images, out);
            ui.add_space(20.0);
            stats_strip(ui, catalog, epic_catalog, library);
            ui.add_space(30.0);

            let xbox_top: Vec<&Game> = catalog.top_games(12).iter().copied().collect();
            rail(
                ui,
                "Popular on Xbox Game Pass",
                "The most-rated titles in the catalog",
                &xbox_top,
                Some(Page::Catalog),
                "rail_xbox",
                images,
                out,
            );
            let epic_recent = top_k(epic_catalog.games.iter(), 12, |a, b| {
                b.release_date_sort
                    .cmp(&a.release_date_sort)
                    .then_with(|| a.name.cmp(&b.name))
            });
            rail(
                ui,
                "Recent Epic Games releases",
                "The latest additions to the Epic store",
                &epic_recent,
                Some(Page::Catalog),
                "rail_epic",
                images,
                out,
            );
            let new_games = top_k(
                catalog
                    .games
                    .iter()
                    .filter(|game| !game.date_added.is_empty()),
                12,
                |a, b| b.date_added.cmp(&a.date_added),
            );
            rail(
                ui,
                "New on Game Pass",
                "Recently added to the catalog",
                &new_games,
                None,
                "rail_new",
                images,
                out,
            );
        });
}

// ───────────────────────── À la une ─────────────────────────

/// Jeux mis en avant : les trois premiers Xbox puis les deux plus récents Epic.
fn featured_games<'a>(catalog: &'a Catalog, epic_catalog: &'a Catalog) -> Vec<&'a Game> {
    let mut games: Vec<&Game> = catalog.top_games(3).iter().copied().collect();
    games.extend(top_k(epic_catalog.games.iter(), 2, |a, b| {
        b.release_date_sort.cmp(&a.release_date_sort)
    }));
    games
}

fn hero(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    epic_catalog: &Catalog,
    images: &mut ImageCache,
    out: &mut Vec<Action>,
) {
    let featured = featured_games(catalog, epic_catalog);
    if featured.is_empty() {
        return;
    }
    for game in &featured {
        images.request(ui.ctx(), &game.id, &game.image, (960, 540));
    }

    // Carrousel : index courant et instant du dernier changement, gardés dans la mémoire egui.
    let state_id = egui::Id::new("home_hero_state");
    let now = ui.input(|i| i.time);
    let (mut index, mut since): (usize, f64) =
        ui.ctx().data(|d| d.get_temp(state_id)).unwrap_or((0, now));
    index %= featured.len();
    if now - since > HERO_ROTATE_SECS {
        index = (index + 1) % featured.len();
        since = now;
    }
    ui.ctx().request_repaint_after(Duration::from_millis(500));

    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), HERO_HEIGHT),
        egui::Sense::hover(),
    );
    let game = featured[index];
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 8.0, theme::BG_CARD);
    if let Some(texture) = images.get(&game.id) {
        images::paint_cover(ui, texture, rect, egui::CornerRadius::same(8), 1.0);
    }

    // Voile : opaque à gauche pour la lisibilité du texte, puis fondu vers le bas.
    let app = theme::BG_APP;
    let opaque = Color32::from_rgba_unmultiplied(app.r(), app.g(), app.b(), 245);
    let clear = Color32::from_rgba_unmultiplied(app.r(), app.g(), app.b(), 0);
    let shade = Color32::from_rgba_unmultiplied(app.r(), app.g(), app.b(), 170);
    gradient(&painter, rect, [opaque, clear, clear, opaque]);
    let bottom = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.center().y), rect.max);
    gradient(&painter, bottom, [clear, clear, app, app]);
    gradient(&painter, rect, [shade, shade, clear, clear]);

    let left = rect.left() + 44.0;
    let (store_label, store_color) = store_style(game.store);
    painter.text(
        egui::pos2(left, rect.bottom() - 178.0),
        egui::Align2::LEFT_TOP,
        store_label,
        egui::FontId::proportional(11.0),
        store_color,
    );
    painter.text(
        egui::pos2(left, rect.bottom() - 156.0),
        egui::Align2::LEFT_TOP,
        game.display_name(),
        egui::FontId::proportional(38.0),
        theme::TEXT_TITLE,
    );
    let meta = [game.publisher.as_str(), game.release.as_str()]
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("  ·  ");
    painter.text(
        egui::pos2(left, rect.bottom() - 100.0),
        egui::Align2::LEFT_TOP,
        meta,
        egui::FontId::proportional(13.0),
        theme::TEXT_MUTED,
    );
    let summary = if game.description.is_empty() {
        String::new()
    } else {
        game.description.chars().take(160).collect()
    };
    widgets::paint_truncated(
        &painter,
        egui::pos2(left, rect.bottom() - 74.0),
        &summary,
        egui::FontId::proportional(12.5),
        theme::TEXT_MUTED,
        560.0,
    );

    let button_rect = egui::Rect::from_min_size(
        egui::pos2(left, rect.bottom() - 46.0),
        egui::vec2(170.0, 36.0),
    );
    let button = egui::Button::new(RichText::new("View details").strong().color(Color32::WHITE))
        .fill(theme::ACCENT)
        .corner_radius(6.0)
        .min_size(button_rect.size());
    if ui
        .put(button_rect, button)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        out.push(Action::OpenGame {
            id: game.id.clone(),
            store: game.store,
        });
    }

    // Pastilles de navigation, en bas à droite.
    let count = featured.len();
    let mut chosen = None;
    for dot in 0..count {
        let center = egui::pos2(
            rect.right() - 36.0 - (count - 1 - dot) as f32 * 20.0,
            rect.bottom() - 28.0,
        );
        let response = ui.interact(
            egui::Rect::from_center_size(center, egui::vec2(16.0, 16.0)),
            ui.id().with(("hero_dot", dot)),
            egui::Sense::click(),
        );
        let active = dot == index;
        painter.circle_filled(
            center,
            if active { 5.0 } else { 4.0 },
            if active {
                theme::ACCENT
            } else {
                theme::BORDER_STRONG
            },
        );
        if response.clicked() {
            chosen = Some(dot);
        }
    }
    if let Some(dot) = chosen {
        index = dot;
        since = now;
    }
    ui.ctx()
        .data_mut(|d| d.insert_temp(state_id, (index, since)));
}

// ───────────────────────── Rangées ─────────────────────────

fn rail(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: &str,
    games: &[&Game],
    see_all: Option<Page>,
    id: &str,
    images: &mut ImageCache,
    out: &mut Vec<Action>,
) {
    if games.is_empty() {
        return;
    }

    // En-tête
    ui.horizontal(|ui| {
        ui.add_space(4.0);

        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(
                RichText::new(title)
                    .size(18.0)
                    .strong()
                    .color(theme::TEXT_TITLE),
            );
            ui.label(widgets::muted(subtitle, 11.5));
        });

        if let Some(page) = see_all {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::pill_button(ui, "See all", None, None, true).clicked() {
                    out.push(Action::Navigate(page));
                }
            });
        }
    });

    ui.add_space(10.0);

    // Défilement horizontal avec barre visible et hauteur compacte.
    egui::ScrollArea::horizontal()
        .id_salt(id)
        .auto_shrink([false, true])
        .max_height(CARD_SIZE.y + 18.0)
        .scroll_bar_visibility(
            egui::containers::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
        )
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = RAIL_GAP;

            ui.horizontal(|ui| {
                ui.add_space(4.0);

                for game in games {
                    images.request(ui.ctx(), &game.id, &game.image, (480, 270));

                    if card(ui, game, images.get(&game.id)) {
                        out.push(Action::OpenGame {
                            id: game.id.clone(),
                            store: game.store,
                        });
                    }
                }
            });
        });

    ui.add_space(24.0);
}

/// Carte paysage : jaquette, titre et éditeur. Retourne `true` au clic.
fn card(ui: &mut egui::Ui, game: &Game, texture: Option<&egui::TextureHandle>) -> bool {
    let (rect, response) = ui.allocate_exact_size(CARD_SIZE, egui::Sense::click());
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("card-hover"), response.hovered(), 0.14);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 6.0, theme::mix((22, 26, 34), (30, 35, 46), hover));

    let art = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), CARD_ART_HEIGHT));
    let radius = egui::CornerRadius {
        nw: 6,
        ne: 6,
        sw: 0,
        se: 0,
    };
    match texture {
        Some(texture) => images::paint_cover(ui, texture, art, radius, 1.0 + 0.04 * hover),
        None => images::paint_placeholder(ui, art, radius, game.display_name()),
    }

    let (store_label, store_color) = store_style(game.store);
    let label_width = painter
        .layout_no_wrap(
            store_label.to_owned(),
            egui::FontId::proportional(9.5),
            store_color,
        )
        .size()
        .x;
    let badge = egui::Rect::from_min_size(
        rect.min + egui::vec2(8.0, 8.0),
        egui::vec2(label_width + 16.0, 20.0),
    );
    painter.rect_filled(badge, 4.0, Color32::from_rgba_unmultiplied(8, 10, 14, 220));
    painter.text(
        badge.center(),
        egui::Align2::CENTER_CENTER,
        store_label,
        egui::FontId::proportional(9.5),
        store_color,
    );

    let text_width = rect.width() - 24.0;
    widgets::paint_truncated(
        &painter,
        egui::pos2(rect.left() + 12.0, art.bottom() + 12.0),
        game.display_name(),
        egui::FontId::proportional(13.5),
        theme::TEXT_TITLE,
        text_width,
    );
    let subtitle = if game.publisher.is_empty() {
        store_label
    } else {
        game.publisher.as_str()
    };
    widgets::paint_truncated(
        &painter,
        egui::pos2(rect.left() + 12.0, art.bottom() + 34.0),
        subtitle,
        egui::FontId::proportional(11.0),
        theme::TEXT_SUBTLE,
        text_width,
    );

    painter.rect_stroke(
        rect,
        6.0,
        egui::Stroke::new(1.0, theme::BORDER.lerp_to_gamma(theme::ACCENT, hover)),
        egui::StrokeKind::Inside,
    );
    response.clicked()
}

// ───────────────────────── Divers ─────────────────────────

fn empty_state(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    epic_catalog: &Catalog,
    out: &mut Vec<Action>,
) {
    ui.add_space(80.0);
    ui.vertical_centered(|ui| {
        if catalog.is_loading() || epic_catalog.is_loading() {
            ui.spinner();
            ui.label(RichText::new("Loading games…").color(theme::TEXT_MUTED));
        } else {
            ui.label(
                RichText::new("No games available. Check your connection and refresh the catalog.")
                    .color(theme::TEXT_MUTED),
            );
            if widgets::pill_button(ui, "Open catalog", None, None, true).clicked() {
                out.push(Action::Navigate(Page::Catalog));
            }
        }
    });
}

/// Bandeau de chiffres clés, juste sous la bannière.
fn stats_strip(ui: &mut egui::Ui, catalog: &Catalog, epic_catalog: &Catalog, library: &Library) {
    egui::Frame::new()
        .fill(theme::BG_CARD)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(20, 14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                stat(
                    ui,
                    "Games available",
                    (catalog.games.len() + epic_catalog.games.len()).to_string(),
                );
                ui.add_space(48.0);
                stat(ui, "Games installed", library.entries.len().to_string());
                ui.add_space(48.0);
                stat(ui, "Space used", format_size(library.total_bytes()));
            });
        });
}

fn stat(ui: &mut egui::Ui, label: &str, value: String) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(
            RichText::new(value)
                .size(18.0)
                .strong()
                .color(theme::TEXT_TITLE),
        );
        ui.label(widgets::muted(label, 11.0));
    });
}

fn store_style(store: GameStore) -> (&'static str, Color32) {
    match store {
        GameStore::Xbox => ("XBOX GAME PASS", theme::GREEN),
        GameStore::Epic => ("EPIC GAMES", theme::EPIC_BLUE),
    }
}

/// Dégradé à quatre coins [haut-gauche, haut-droit, bas-droit, bas-gauche], peint en maillage.
fn gradient(painter: &egui::Painter, rect: egui::Rect, corners: [Color32; 4]) {
    let mut mesh = Mesh::default();
    let points = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    for (pos, color) in points.into_iter().zip(corners) {
        mesh.vertices.push(Vertex {
            pos,
            uv: WHITE_UV,
            color,
        });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}
