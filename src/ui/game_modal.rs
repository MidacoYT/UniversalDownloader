//! Fenêtre modale de détails d'un jeu (hero, stats, médias, fonctionnalités).
use super::{Action, icons, theme, widgets};
use crate::images::{self, ImageCache};
use crate::model::{FeatureGroup, Game, GameAttribute, GameStore};
use crate::util::{format_count, open_store_page};
use eframe::egui::{self, Color32, RichText, TextureHandle};
use fluent_egui::winui::prelude::*;

const MEDIA_MAX: (u32, u32) = (960, 540);
const HERO_HEIGHT: f32 = 205.0;

/// Position des carrousels (réinitialisée à l'ouverture d'un jeu).
#[derive(Default)]
pub struct ModalState {
    pub screenshot: usize,
    pub trailer: usize,
}

/// Textures déjà chargées pour le jeu affiché.
struct Media {
    hero: Option<TextureHandle>,
    screenshot: Option<TextureHandle>,
    trailer: Option<TextureHandle>,
}

impl Media {
    fn load(ctx: &egui::Context, game: &Game, state: &ModalState, images: &mut ImageCache) -> Self {
        let hero_key = images::hero_key(&game.id);
        images.request(ctx, &hero_key, &game.hero, MEDIA_MAX);

        let shot_key = format!("screenshot:{}:{}", game.id, state.screenshot);
        let shot_url = game
            .screenshots
            .get(state.screenshot)
            .map(String::as_str)
            .unwrap_or("");
        images.request(ctx, &shot_key, shot_url, MEDIA_MAX);

        let trailer_key = format!("trailer:{}:{}", game.id, state.trailer);
        let trailer_url = game
            .trailers
            .get(state.trailer)
            .map(|t| t.preview.as_str())
            .unwrap_or("");
        images.request(ctx, &trailer_key, trailer_url, MEDIA_MAX);

        Self {
            hero: images
                .get(&hero_key)
                .or_else(|| images.get(&game.id))
                .cloned(),
            screenshot: images.get(&shot_key).cloned(),
            trailer: images.get(&trailer_key).cloned(),
        }
    }
}

pub fn show(
    ctx: &egui::Context,
    game: &Game,
    state: &mut ModalState,
    images: &mut ImageCache,
    interactive: bool,
    out: &mut Vec<Action>,
) {
    state.screenshot = state
        .screenshot
        .min(game.screenshots.len().saturating_sub(1));
    state.trailer = state.trailer.min(game.trailers.len().saturating_sub(1));

    let viewport = ctx.input(|i| i.viewport_rect());
    let width = (viewport.width() - 48.0).clamp(420.0, 820.0);
    let height = (viewport.height() - 48.0).clamp(360.0, 650.0);
    let rect = egui::Rect::from_center_size(viewport.center(), egui::vec2(width, height));

    ctx.layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::new("game_modal_backdrop"),
    ))
    .rect_filled(viewport, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 195));

    let media = Media::load(ctx, game, state, images);
    // Fermeture clavier / clic extérieur désactivée tant que le lecteur de trailer est ouvert.
    let mut close = interactive
        && ctx.input(|i| {
            i.key_pressed(egui::Key::Escape)
                || (i.pointer.any_click()
                    && i.pointer.interact_pos().is_some_and(|p| !rect.contains(p)))
        });

    egui::Area::new(egui::Id::new("game_details_modal"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .show(ctx, |ui| {
            ui.set_min_size(rect.size());
            ui.set_max_size(rect.size());
            egui::Frame::new()
                .fill(theme::BG_MODAL)
                .stroke(egui::Stroke::new(
                    1.0,
                    Color32::from_rgba_unmultiplied(168, 85, 247, 100),
                ))
                .shadow(egui::epaint::Shadow {
                    offset: [0, 12],
                    blur: 42,
                    spread: 0,
                    color: Color32::from_rgba_unmultiplied(0, 0, 0, 180),
                })
                .corner_radius(egui::CornerRadius::same(16))
                .inner_margin(egui::Margin::same(0))
                .show(ui, |ui| {
                    ui.set_min_width(rect.width());
                    ui.set_max_width(rect.width());
                    if hero(ui, game, media.hero.as_ref()) {
                        close = true;
                    }
                    egui::ScrollArea::vertical()
                        .max_height(ui.available_height() - 4.0)
                        .show(ui, |ui| {
                            egui::Frame::new()
                                .inner_margin(egui::Margin::same(20))
                                .show(ui, |ui| body(ui, game, state, &media, out));
                        });
                });
        });

    if close {
        out.push(Action::CloseGame);
    }
}

// ───────────────────────── Hero ─────────────────────────

/// Bandeau supérieur ; retourne `true` si le bouton de fermeture est cliqué.
fn hero(ui: &mut egui::Ui, game: &Game, texture: Option<&TextureHandle>) -> bool {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), HERO_HEIGHT),
        egui::Sense::hover(),
    );
    let radius = images::top_radius(15);
    match texture {
        Some(texture) => images::paint_cover(ui, texture, rect, radius, 1.0),
        None => {
            ui.painter()
                .rect_filled(rect, radius, Color32::from_rgb(31, 25, 48));
        }
    }
    let painter = ui.painter();
    painter.rect_filled(rect, radius, Color32::from_rgba_unmultiplied(8, 8, 15, 48));
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(rect.left(), rect.center().y - 12.0),
            rect.right_bottom(),
        ),
        0.0,
        Color32::from_rgba_unmultiplied(8, 8, 15, 190),
    );

    let badge = egui::Rect::from_min_size(
        rect.left_bottom() + egui::vec2(20.0, -82.0),
        egui::vec2(112.0, 22.0),
    );
    painter.rect_filled(badge, 6.0, theme::GREEN);
    painter.text(
        badge.center(),
        egui::Align2::CENTER_CENTER,
        if game.ea_play { "EA PLAY" } else { "GAME PASS" },
        egui::FontId::proportional(10.0),
        Color32::WHITE,
    );
    painter.text(
        rect.left_bottom() + egui::vec2(20.0, -28.0),
        egui::Align2::LEFT_BOTTOM,
        game.display_name(),
        egui::FontId::proportional(25.0),
        Color32::from_rgb(245, 243, 255),
    );
    painter.text(
        rect.left_bottom() + egui::vec2(20.0, -9.0),
        egui::Align2::LEFT_BOTTOM,
        format!(
            "{}  •  {}",
            or_fallback(&game.developer, "Unknown developer"),
            or_fallback(&game.publisher, "Unknown publisher")
        ),
        egui::FontId::proportional(12.0),
        Color32::from_rgb(190, 186, 207),
    );

    let close_rect = egui::Rect::from_min_size(
        rect.right_top() + egui::vec2(-44.0, 12.0),
        egui::vec2(32.0, 32.0),
    );
    let response = ui.interact(
        close_rect,
        ui.id().with("modal_close"),
        egui::Sense::click(),
    );
    painter.circle_filled(
        close_rect.center(),
        16.0,
        if response.hovered() {
            Color32::from_rgba_unmultiplied(255, 255, 255, 60)
        } else {
            Color32::from_rgba_unmultiplied(0, 0, 0, 150)
        },
    );
    painter.text(
        close_rect.center(),
        egui::Align2::CENTER_CENTER,
        "×",
        egui::FontId::proportional(18.0),
        Color32::WHITE,
    );
    response.clicked()
}

// ───────────────────────── Corps ─────────────────────────

fn body(
    ui: &mut egui::Ui,
    game: &Game,
    state: &mut ModalState,
    media: &Media,
    out: &mut Vec<Action>,
) {
    stats_row(ui, game);

    heading(ui, "Description");
    egui::Frame::new()
        .fill(theme::BG_CARD)
        .corner_radius(10)
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.label(or_fallback(&game.description, "No description available."));
        });

    heading(ui, "Additional Information");
    info_cards(ui, game);
    links(ui, game);
    genres(ui, game);

    if !game.screenshots.is_empty() {
        let count = game.screenshots.len();
        heading(ui, &format!("Screenshots ({count})"));
        carousel(
            ui,
            count,
            &mut state.screenshot,
            media.screenshot.as_ref(),
            250.0,
            "Loading screenshot…",
            true,
            false,
        );
    }
    if !game.trailers.is_empty() {
        let count = game.trailers.len();
        heading(ui, &format!("Trailers ({count})"));
        let playable = game
            .trailers
            .get(state.trailer)
            .is_some_and(|t| !t.url.is_empty());
        let play = carousel(
            ui,
            count,
            &mut state.trailer,
            media.trailer.as_ref(),
            220.0,
            "Trailer available",
            false,
            playable,
        );
        if let Some(trailer) = game.trailers.get(state.trailer) {
            if !trailer.caption.is_empty() {
                ui.label(widgets::muted(&trailer.caption, 12.0).italics());
            }
            if play {
                let title = if trailer.caption.is_empty() {
                    game.display_name().to_owned()
                } else {
                    trailer.caption.clone()
                };
                out.push(Action::PlayTrailer {
                    title,
                    url: trailer.url.clone(),
                });
            }
        }
    }

    game_information(ui, game);
    footer(ui, game, out);
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(20.0);
    ui.label(RichText::new(text).size(17.0).strong());
    ui.add_space(8.0);
}

fn or_fallback<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() { fallback } else { value }
}

fn stats_row(ui: &mut egui::Ui, game: &Game) {
    let reviews = if game.rating_count == 0 {
        "New game".to_owned()
    } else {
        format!("{} reviews", format_count(game.rating_count))
    };
    let stats = [
        (icons::star(14.0), reviews),
        (
            icons::players(14.0),
            or_fallback(&game.players, "—").to_owned(),
        ),
        (
            icons::storage(14.0),
            or_fallback(&game.size, "—").to_owned(),
        ),
        (
            icons::calendar(14.0),
            or_fallback(&game.release, "N/A").to_owned(),
        ),
        (
            icons::shield(14.0),
            or_fallback(&game.rating, "—").to_owned(),
        ),
    ];
    let gap = 8.0;
    let width = (ui.available_width() - gap * 4.0).max(0.0) / 5.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (image, value) in stats {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 36.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 10.0, Color32::from_rgb(30, 30, 45));
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(rect.shrink2(egui::vec2(6.0, 0.0)))
                    .layout(
                        egui::Layout::left_to_right(egui::Align::Center)
                            .with_main_align(egui::Align::Center),
                    ),
                |cell| {
                    cell.add(image);
                    cell.add_space(5.0);
                    cell.label(RichText::new(value).size(11.0).strong());
                },
            );
        }
    });
}

fn info_cards(ui: &mut egui::Ui, game: &Game) {
    let mut right = vec![("Release date", game.release.as_str())];
    if !game.platform.is_empty() {
        right.push(("Platforms", game.platform.as_str()));
    }
    ui.columns(2, |columns| {
        info_card(
            &mut columns[0],
            &[
                ("Publisher", &game.publisher),
                ("Developer", &game.developer),
            ],
        );
        info_card(&mut columns[1], &right);
    });
}

fn info_card(ui: &mut egui::Ui, rows: &[(&str, &str)]) {
    egui::Frame::new()
        .fill(theme::BG_CARD)
        .stroke(egui::Stroke::new(1.0, theme::accent_alpha(50)))
        .corner_radius(8)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            for (index, (label, value)) in rows.iter().enumerate() {
                if index > 0 {
                    ui.add_space(12.0);
                }
                ui.label(widgets::muted(*label, 11.0));
                ui.label(or_fallback(value, "—"));
            }
        });
}

fn links(ui: &mut egui::Ui, game: &Game) {
    if game.publisher_website.is_empty()
        && game.support_uri.is_empty()
        && game.language.is_empty()
        && game.last_modified.is_empty()
    {
        return;
    }
    ui.add_space(12.0);
    ui.horizontal_wrapped(|ui| {
        if !game.publisher_website.is_empty() {
            ui.hyperlink_to("Website", &game.publisher_website);
        }
        if !game.support_uri.is_empty() {
            ui.hyperlink_to("Get Help", &game.support_uri);
        }
        if !game.language.is_empty() {
            ui.label(format!("Language: {}", game.language.to_uppercase()));
        }
        if !game.last_modified.is_empty() {
            ui.label(format!("Last updated: {}", game.last_modified));
        }
    });
}

fn genres(ui: &mut egui::Ui, game: &Game) {
    if game.genres.is_empty() {
        return;
    }
    ui.add_space(18.0);
    ui.horizontal_wrapped(|ui| {
        for genre in &game.genres {
            egui::Frame::new()
                .fill(theme::accent_alpha(48))
                .stroke(egui::Stroke::new(1.0, theme::accent_alpha(78)))
                .corner_radius(20)
                .inner_margin(egui::Margin::symmetric(10, 5))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(genre)
                            .size(11.0)
                            .color(Color32::from_rgb(196, 181, 253)),
                    );
                });
        }
    });
}

// ───────────────────────── Carrousel ─────────────────────────

fn carousel(
    ui: &mut egui::Ui,
    count: usize,
    index: &mut usize,
    texture: Option<&TextureHandle>,
    height: f32,
    placeholder: &str,
    dots: bool,
    playable: bool,
) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        let image_size = egui::vec2((ui.available_width() - 88.0).max(160.0), height);
        let arrow_size = egui::vec2(36.0, height);
        if carousel_arrow(ui, arrow_size, "‹", count > 1).clicked() {
            *index = (*index + count - 1) % count;
        }
        clicked = carousel_image(ui, texture, image_size, placeholder, playable);
        if carousel_arrow(ui, arrow_size, "›", count > 1).clicked() {
            *index = (*index + 1) % count;
        }
    });
    if dots {
        carousel_dots(ui, count, index);
    }
    clicked
}

/// Image du carrousel ; si `playable`, un bouton lecture est superposé et l'image est cliquable.
/// Retourne `true` quand l'utilisateur clique pour lancer la lecture.
fn carousel_image(
    ui: &mut egui::Ui,
    texture: Option<&TextureHandle>,
    size: egui::Vec2,
    placeholder: &str,
    playable: bool,
) -> bool {
    let sense = if playable {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    match texture {
        Some(texture) => {
            ui.painter().rect_filled(rect, 8.0, theme::BG_CARD);
            // Mode « contain » : l'image entière reste visible.
            let original = texture.size_vec2();
            let scale = (size.x / original.x).min(size.y / original.y);
            let fitted = egui::Rect::from_center_size(rect.center(), original * scale);
            ui.painter().image(
                texture.id(),
                fitted,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        None if playable => {
            ui.painter().rect_filled(rect, 8.0, theme::BG_CARD);
        }
        None => images::paint_placeholder(ui, rect, egui::CornerRadius::same(8), placeholder),
    }
    if !playable {
        return false;
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    paint_play_button(ui, rect, response.hovered());
    response.clicked()
}

/// Bouton lecture rond (triangle blanc) centré sur `rect`.
fn paint_play_button(ui: &egui::Ui, rect: egui::Rect, hovered: bool) {
    let center = rect.center();
    let radius = if hovered { 32.0 } else { 28.0 };
    let painter = ui.painter();
    painter.circle_filled(
        center,
        radius,
        if hovered {
            theme::ACCENT
        } else {
            Color32::from_rgba_unmultiplied(10, 10, 18, 190)
        },
    );
    painter.circle_stroke(
        center,
        radius,
        egui::Stroke::new(
            2.0,
            Color32::from_white_alpha(if hovered { 230 } else { 150 }),
        ),
    );
    let s = radius * 0.42;
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(center.x - s * 0.6, center.y - s),
            egui::pos2(center.x - s * 0.6, center.y + s),
            egui::pos2(center.x + s, center.y),
        ],
        Color32::WHITE,
        egui::Stroke::NONE,
    ));
}

fn carousel_arrow(
    ui: &mut egui::Ui,
    slot_size: egui::Vec2,
    label: &str,
    enabled: bool,
) -> egui::Response {
    let (slot, _) = ui.allocate_exact_size(slot_size, egui::Sense::hover());
    let button_size = egui::vec2(32.0, 32.0);
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(slot)
            .layout(egui::Layout::top_down(egui::Align::Center)),
        |slot_ui| {
            slot_ui.add_space(((slot_size.y - button_size.y) * 0.5).max(0.0));
            slot_ui
                .add_enabled_ui(enabled, |ui| {
                    ui.add_sized(button_size, egui::Button::new(label))
                })
                .inner
        },
    )
    .inner
}

fn carousel_dots(ui: &mut egui::Ui, count: usize, index: &mut usize) {
    let dots = count.min(10);
    ui.horizontal(|ui| {
        let total = dots as f32 * 8.0 + dots.saturating_sub(1) as f32 * ui.spacing().item_spacing.x;
        ui.add_space((ui.available_width() - total).max(0.0) * 0.5);
        for i in 0..dots {
            let (dot, response) =
                ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::click());
            let color = if i == *index {
                theme::ACCENT
            } else {
                theme::accent_alpha(80)
            };
            ui.painter().circle_filled(dot.center(), 4.0, color);
            if response.clicked() {
                *index = i;
            }
        }
    });
}

// ───────────────────────── Infos de jeu ─────────────────────────

fn game_information(ui: &mut egui::Ui, game: &Game) {
    if game.achievements.is_none()
        && game.gamerscore.is_none()
        && game.categories.is_empty()
        && game.attributes.is_empty()
    {
        return;
    }
    heading(ui, "Game Information");
    ui.columns(3, |columns| {
        if let Some(achievements) = game.achievements {
            stat_block(
                &mut columns[0],
                "Achievements",
                achievements.to_string(),
                theme::ACCENT,
            );
        }
        if let Some(gamerscore) = game.gamerscore {
            stat_block(
                &mut columns[1],
                "Gamerscore",
                format!("{gamerscore} G"),
                theme::GOLD,
            );
        }
        if !game.categories.is_empty() {
            columns[2].label(RichText::new("Categories").size(11.0).color(theme::CYAN));
            columns[2].label(game.categories.join(" · "));
        }
    });
    if !game.attributes.is_empty() {
        ui.add_space(14.0);
        features(ui, game);
    }
}

fn stat_block(ui: &mut egui::Ui, label: &str, value: String, color: Color32) {
    ui.label(RichText::new(label).size(11.0).color(color));
    ui.label(RichText::new(value).size(24.0).strong().color(color));
}

fn features(ui: &mut egui::Ui, game: &Game) {
    egui::Frame::new()
        .fill(theme::BG_CARD)
        .stroke(egui::Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(16, 185, 129, 70),
        ))
        .corner_radius(8)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.label(
                RichText::new("Features")
                    .size(13.0)
                    .strong()
                    .color(theme::EMERALD),
            );
            ui.label(widgets::muted("Capabilities & compatibility", 11.0));
            ui.add_space(8.0);
            // Deux groupes par colonne : (Multiplayer, Graphics) | (Compatibility, Xbox Live).
            ui.columns(2, |columns| {
                for (column, groups) in columns.iter_mut().zip(FeatureGroup::ALL.chunks(2)) {
                    for &group in groups {
                        let entries: Vec<&GameAttribute> = game
                            .attributes
                            .iter()
                            .filter(|a| a.group() == group)
                            .collect();
                        if entries.is_empty() {
                            continue;
                        }
                        feature_box(column, group, &entries);
                        column.add_space(8.0);
                    }
                }
            });
        });
}

fn feature_box(ui: &mut egui::Ui, group: FeatureGroup, entries: &[&GameAttribute]) {
    egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(16, 185, 129, 20))
        .stroke(egui::Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(16, 185, 129, 45),
        ))
        .corner_radius(8)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |section| {
            section.set_min_width(section.available_width());
            section.label(
                RichText::new(group.title())
                    .size(11.0)
                    .strong()
                    .color(Color32::from_rgb(167, 243, 208)),
            );
            section.add_space(4.0);
            for attribute in entries {
                section.horizontal_wrapped(|row| {
                    row.label(RichText::new("•").color(theme::EMERALD));
                    row.label(
                        RichText::new(attribute.label())
                            .size(11.0)
                            .color(Color32::from_rgb(220, 230, 230)),
                    );
                });
            }
        });
}

fn footer(ui: &mut egui::Ui, game: &Game, out: &mut Vec<Action>) {
    ui.add_space(18.0);
    ui.horizontal(|ui| {
        match game.store {
            GameStore::Xbox => {
                if ui.add(FluentButton::accent("Download")).clicked() {
                    out.push(Action::PrepareDownload {
                        id: game.id.clone(),
                        name: game.display_name().to_owned(),
                        store: game.store,
                    });
                }
                if ui.add(FluentButton::accent("Open in Xbox")).clicked() {
                    open_store_page(&game.id);
                }
            }
            GameStore::Epic => {
                if ui.add(FluentButton::accent("Download")).clicked() {
                    out.push(Action::PrepareDownload {
                        id: game.id.clone(),
                        name: game.display_name().to_owned(),
                        store: game.store,
                    });
                }
                if !game.egdata_url.is_empty() && ui.button("More game info").clicked() {
                    let _ = webbrowser::open(&game.egdata_url);
                }
                if !game.manifest_url.is_empty() && ui.button("Copy manifest URL").clicked() {
                    ui.ctx().copy_text(format!(
                        "{}{}",
                        crate::catalog::EPIC_API_BASE,
                        game.manifest_url
                    ));
                    out.push(Action::EpicCatalogStatus(
                        "Epic manifest URL copied.".into(),
                    ));
                }
            }
        }
        if ui.button("Copy ID").clicked() {
            ui.ctx().copy_text(game.id.clone());
            out.push(match game.store {
                GameStore::Xbox => Action::CatalogStatus("ID copied to clipboard.".into()),
                GameStore::Epic => {
                    Action::EpicCatalogStatus("Epic appName copied to clipboard.".into())
                }
            });
        }
    });
}
