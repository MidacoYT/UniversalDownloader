//! Fiche jeu « Connect » : bannière plein format, actions en évidence, puis deux colonnes
//! (contenu à gauche, informations à droite).
use super::{Action, theme, topbar, widgets};
use crate::images::{self, ImageCache};
use crate::model::{FeatureGroup, Game, GameAttribute, GameStore};
use crate::util::{format_count, open_store_page};
use eframe::egui::epaint::{Mesh, Vertex, WHITE_UV};
use eframe::egui::{self, Color32, RichText, TextureHandle};

const MEDIA_MAX: (u32, u32) = (960, 540);
const HERO_HEIGHT: f32 = 230.0;
const SIDE_WIDTH: f32 = 340.0;
const GUTTER: f32 = 32.0;

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

    // Zone réellement visible : sous la barre de titre personnalisée, jusqu'aux bords.
    let viewport = ctx.input(|i| i.viewport_rect());
    let content = egui::Rect::from_min_max(
        egui::pos2(viewport.left(), viewport.top() + topbar::HEIGHT),
        viewport.max,
    );
    // La modale utilise presque tout le viewport. Seule la largeur est plafonnée
    // sur les écrans géants ; la hauteur suit toujours la fenêtre.
    const MARGIN: f32 = 16.0;
    let width = (content.width() - MARGIN * 2.0).max(280.0).min(1600.0);
    let height = (content.height() - MARGIN * 2.0).max(260.0);
    let rect = egui::Rect::from_center_size(content.center(), egui::vec2(width, height));

    ctx.layer_painter(egui::LayerId::new(
        egui::Order::Middle,
        egui::Id::new("game_modal_backdrop"),
    ))
    .rect_filled(content, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 170));

    let media = Media::load(ctx, game, state, images);
    let mut close = interactive
        && ctx.input(|i| {
            i.key_pressed(egui::Key::Escape)
                || (i.pointer.any_click()
                    && i.pointer.interact_pos().is_some_and(|p| !rect.contains(p)))
        });

    // Zone de la modale : layout borné au rectangle + clip, pour que rien ne sorte du cadre.
    egui::Area::new(egui::Id::new("game_details_modal"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .default_size(rect.size())
        .show(ctx, |ui| {
            // Important : Area est normalement dimensionnée par son contenu.
            // On lui impose ici la taille du viewport pour éviter l'effet carré
            // et empêcher le contenu de réduire la modale.
            ui.set_min_size(rect.size());
            ui.set_max_size(rect.size());
            ui.set_width(rect.width());
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                ui.set_min_size(rect.size());
                ui.set_max_size(rect.size());
                ui.set_clip_rect(rect);
                egui::Frame::new()
                    .fill(theme::BG_MODAL)
                    .stroke(egui::Stroke::new(1.0, theme::BORDER_STRONG))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 12],
                        blur: 42,
                        spread: 0,
                        color: Color32::from_rgba_unmultiplied(0, 0, 0, 180),
                    })
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::same(0))
                    .show(ui, |ui| {
                        if hero(ui, game, media.hero.as_ref(), out) {
                            close = true;
                        }
                        egui::ScrollArea::vertical()
                            .id_salt(format!("game_modal_scroll:{}", game.id))
                            .max_height(ui.available_height().max(0.0))
                            .max_width(ui.available_width())
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                egui::Frame::new()
                                    .inner_margin(egui::Margin::symmetric(
                                        if ui.available_width() < 760.0 { 16 } else { 28 },
                                        24,
                                    ))
                                    .show(ui, |ui| body(ui, game, state, &media, out));
                            });
                    });
            });
        });

    if close {
        out.push(Action::CloseGame);
    }
}

// ───────────────────────── Bannière ─────────────────────────

/// Bannière plein format avec titre, actions principales et bouton de fermeture.
/// Retourne `true` si le bouton de fermeture est cliqué.
fn hero(
    ui: &mut egui::Ui,
    game: &Game,
    texture: Option<&TextureHandle>,
    out: &mut Vec<Action>,
) -> bool {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), HERO_HEIGHT),
        egui::Sense::hover(),
    );
    let radius = egui::CornerRadius {
        nw: 8,
        ne: 8,
        sw: 0,
        se: 0,
    };
    let painter = ui.painter().clone();
    painter.rect_filled(rect, radius, theme::BG_CARD);
    if let Some(texture) = texture {
        images::paint_cover(ui, texture, rect, radius, 1.0);
    }

    // Voiles : fondu vers le bas pour le texte, et ombrage à gauche.
    let app = theme::BG_APP;
    let clear = Color32::from_rgba_unmultiplied(app.r(), app.g(), app.b(), 0);
    let solid = Color32::from_rgba_unmultiplied(app.r(), app.g(), app.b(), 255);
    let shade = Color32::from_rgba_unmultiplied(app.r(), app.g(), app.b(), 190);
    let bottom = egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.top() + rect.height() * 0.25),
        rect.max,
    );
    gradient(&painter, bottom, [clear, clear, solid, solid]);
    gradient(&painter, rect, [shade, clear, clear, shade]);

    let left = rect.left() + 44.0;
    let (store_label, store_color) = match game.store {
        GameStore::Xbox => ("XBOX GAME PASS", theme::GREEN),
        GameStore::Epic => ("EPIC GAMES", theme::EPIC_BLUE),
    };
    painter.text(
        egui::pos2(left, rect.bottom() - 118.0),
        egui::Align2::LEFT_TOP,
        store_label,
        egui::FontId::proportional(11.0),
        store_color,
    );
    painter.text(
        egui::pos2(left, rect.bottom() - 98.0),
        egui::Align2::LEFT_TOP,
        game.display_name(),
        egui::FontId::proportional(30.0),
        theme::TEXT_TITLE,
    );
    let developer = or_fallback(&game.developer, "Unknown developer");
    let publisher = or_fallback(&game.publisher, "Unknown publisher");
    painter.text(
        egui::pos2(left, rect.bottom() - 58.0),
        egui::Align2::LEFT_TOP,
        format!("{developer}  ·  {publisher}"),
        egui::FontId::proportional(13.0),
        theme::TEXT_MUTED,
    );

    // Actions principales : téléchargement en bleu, lien secondaire à côté.
    let button_y = rect.bottom() - 38.0;
    let download = egui::Button::new(RichText::new("Download").strong().color(Color32::WHITE))
        .fill(theme::ACCENT)
        .corner_radius(6.0)
        .min_size(egui::vec2(170.0, 36.0));
    if ui
        .put(
            egui::Rect::from_min_size(egui::pos2(left, button_y), egui::vec2(170.0, 36.0)),
            download,
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
    {
        out.push(Action::PrepareDownload {
            id: game.id.clone(),
            name: game.display_name().to_owned(),
            store: game.store,
        });
    }
    match game.store {
        GameStore::Xbox => {
            let secondary =
                egui::Button::new(RichText::new("Open in Xbox").color(theme::TEXT_TITLE))
                    .fill(Color32::from_rgba_unmultiplied(0, 0, 0, 0))
                    .stroke(egui::Stroke::new(1.0, theme::BORDER_STRONG))
                    .corner_radius(6.0)
                    .min_size(egui::vec2(170.0, 36.0));
            if ui
                .put(
                    egui::Rect::from_min_size(
                        egui::pos2(left + 182.0, button_y),
                        egui::vec2(170.0, 36.0),
                    ),
                    secondary,
                )
                .clicked()
            {
                open_store_page(&game.id);
            }
        }
        GameStore::Epic => {
            // Page officielle Epic en priorité ; l'ancien lien tiers ne sert que de secours.
            let info_url = if game.store_url.is_empty() {
                game.egdata_url.as_str()
            } else {
                game.store_url.as_str()
            };
            if !info_url.is_empty() {
                let secondary =
                    egui::Button::new(RichText::new("More info").color(theme::TEXT_TITLE))
                        .fill(Color32::from_rgba_unmultiplied(0, 0, 0, 0))
                        .stroke(egui::Stroke::new(1.0, theme::BORDER_STRONG))
                        .corner_radius(6.0)
                        .min_size(egui::vec2(170.0, 36.0));
                if ui
                    .put(
                        egui::Rect::from_min_size(
                            egui::pos2(left + 182.0, button_y),
                            egui::vec2(170.0, 36.0),
                        ),
                        secondary,
                    )
                    .clicked()
                {
                    let _ = webbrowser::open(info_url);
                }
            }
        }
    }

    // Bouton de fermeture, en haut à droite.
    let close_rect = egui::Rect::from_min_size(
        rect.right_top() + egui::vec2(-48.0, 14.0),
        egui::vec2(34.0, 34.0),
    );
    let response = ui.interact(
        close_rect,
        ui.id().with("modal_close"),
        egui::Sense::click(),
    );
    painter.circle_filled(
        close_rect.center(),
        17.0,
        if response.hovered() {
            Color32::from_rgba_unmultiplied(255, 255, 255, 40)
        } else {
            Color32::from_rgba_unmultiplied(10, 12, 16, 170)
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

// ───────────────────────── Corps : deux colonnes ─────────────────────────

fn body(
    ui: &mut egui::Ui,
    game: &Game,
    state: &mut ModalState,
    media: &Media,
    out: &mut Vec<Action>,
) {
    let total = ui.available_width();
    let gap = 28.0;
    let side = SIDE_WIDTH.min((total * 0.34).max(250.0));

    // Sur une fenêtre étroite, empiler les sections évite de comprimer
    // la description et de faire sortir la colonne Informations du cadre.
    if total < 760.0 {
        main_column(ui, game, state, media, out);
        ui.add_space(28.0);
        side_column(ui, game);
        return;
    }

    let main = (total - side - gap).max(240.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.allocate_ui_with_layout(
            egui::vec2(main, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(main);
                main_column(ui, game, state, media, out);
            },
        );
        ui.add_space(gap);
        ui.allocate_ui_with_layout(
            egui::vec2(side, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(side);
                side_column(ui, game);
            },
        );
    });
}

fn main_column(
    ui: &mut egui::Ui,
    game: &Game,
    state: &mut ModalState,
    media: &Media,
    out: &mut Vec<Action>,
) {
    stats_strip(ui, game);

    heading(ui, "About");
    let description = if game.store == GameStore::Epic && !game.long_description.is_empty() {
        clean_epic_description(&game.long_description)
    } else {
        game.description.clone()
    };
    ui.label(
        RichText::new(or_fallback(&description, "No description available."))
            .size(13.0)
            .color(theme::TEXT_MUTED),
    );

    if !game.screenshots.is_empty() {
        let count = game.screenshots.len();
        heading(ui, &format!("Screenshots ({count})"));
        carousel(
            ui,
            count,
            &mut state.screenshot,
            media.screenshot.as_ref(),
            280.0,
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
            240.0,
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
    ui.add_space(24.0);
}

fn side_column(ui: &mut egui::Ui, game: &Game) {
    heading(ui, "Information");
    info_card(ui, &info_rows(game));
    links(ui, game);
    genres(ui, game);
    if game.store == GameStore::Epic && !game.store_info_status.is_empty() {
        ui.add_space(6.0);
        ui.label(widgets::muted(&game.store_info_status, 11.0));
    }
    game_information(ui, game);
    ui.add_space(24.0);
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(20.0);
    ui.label(
        RichText::new(text)
            .size(16.0)
            .strong()
            .color(theme::TEXT_TITLE),
    );
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::BORDER);
    ui.add_space(12.0);
}

fn or_fallback<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() { fallback } else { value }
}

fn clean_epic_description(value: &str) -> String {
    value
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || (line.starts_with("<!--") && line.ends_with("-->")) {
                return None;
            }
            let clean = line.trim_start_matches('#').trim();
            (!clean.is_empty()).then(|| clean.to_owned())
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Bandeau de chiffres clés : note, joueurs, taille, date, classification.
fn stats_strip(ui: &mut egui::Ui, game: &Game) {
    let reviews = match game.store {
        GameStore::Epic => match game.critic_score {
            Some(score) => format!("OpenCritic {score}/100"),
            None if game.epic_stats_requested && !game.epic_stats_loaded => "Loading…".into(),
            None => "No rating".into(),
        },
        GameStore::Xbox if game.rating_count == 0 => "New".to_owned(),
        GameStore::Xbox => format!("{} reviews", format_count(game.rating_count)),
    };
    let tiles = [
        ("Rating", reviews),
        ("Players", or_fallback(&game.players, "—").to_owned()),
        ("Size", or_fallback(&game.size, "—").to_owned()),
        ("Release", or_fallback(&game.release, "—").to_owned()),
        ("Rating", or_fallback(&game.rating, "—").to_owned()),
    ];
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
        for (label, value) in tiles {
            egui::Frame::new()
                .fill(theme::BG_CARD)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .corner_radius(6)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_max_width(230.0);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(widgets::muted(label, 10.0));
                    // Pas de troncature : la valeur passe à la ligne si elle est longue.
                    ui.add(egui::Label::new(RichText::new(value).size(12.5).strong()).wrap());
                });
        }
    });
}

fn info_rows(game: &Game) -> Vec<(String, String)> {
    let mut rows = vec![
        (
            "Publisher".to_owned(),
            or_fallback(&game.publisher, "—").to_owned(),
        ),
        (
            "Developer".to_owned(),
            or_fallback(&game.developer, "—").to_owned(),
        ),
        (
            "Release date".to_owned(),
            or_fallback(&game.release, "—").to_owned(),
        ),
    ];
    if !game.platform.is_empty() {
        rows.push(("Platforms".to_owned(), game.platform.clone()));
    }
    if let Some(percent) = game.critic_recommend {
        rows.push((
            "Critic reviews".to_owned(),
            format!("OpenCritic recommends · {percent}%"),
        ));
    }
    rows
}

fn info_card(ui: &mut egui::Ui, rows: &[(String, String)]) {
    egui::Frame::new()
        .fill(theme::BG_CARD)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(6)
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            for (index, (label, value)) in rows.iter().enumerate() {
                if index > 0 {
                    ui.add_space(10.0);
                    let (line, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 1.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().rect_filled(line, 0.0, theme::BORDER);
                    ui.add_space(10.0);
                }
                ui.label(widgets::muted(label.as_str(), 10.5));
                ui.label(RichText::new(value.as_str()).size(12.5));
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
        ui.spacing_mut().item_spacing.x = 12.0;
        if !game.publisher_website.is_empty() {
            ui.hyperlink_to("Website", &game.publisher_website);
        }
        if !game.support_uri.is_empty() {
            ui.hyperlink_to("Support", &game.support_uri);
        }
        if !game.language.is_empty() {
            ui.label(widgets::muted(
                format!("Language: {}", game.language.to_uppercase()),
                11.0,
            ));
        }
        if !game.last_modified.is_empty() {
            ui.label(widgets::muted(
                format!("Updated: {}", game.last_modified),
                11.0,
            ));
        }
    });
}

fn genres(ui: &mut egui::Ui, game: &Game) {
    if game.genres.is_empty() {
        return;
    }
    ui.add_space(14.0);
    // Texte simple qui passe à la ligne : pas de chips étroits, donc pas de découpage par lettre.
    ui.add(egui::Label::new(widgets::muted(game.genres.join("  ·  "), 11.5)).wrap());
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
    heading(ui, "About this game");
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
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("Features")
                    .size(13.0)
                    .strong()
                    .color(theme::EMERALD),
            );
            ui.label(widgets::muted("Capabilities and compatibility", 11.0));
            ui.add_space(12.0);

            let available = ui.available_width();
            let gap = 12.0;
            let column_width = ((available - gap) * 0.5).max(120.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (column_index, groups) in FeatureGroup::ALL.chunks(2).enumerate() {
                    if column_index > 0 {
                        ui.add_space(gap);
                    }
                    ui.allocate_ui_with_layout(
                        egui::vec2(column_width, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |column| {
                            column.set_width(column_width);
                            for &group in groups {
                                let entries: Vec<&GameAttribute> = game
                                    .attributes
                                    .iter()
                                    .filter(|attribute| attribute.group() == group)
                                    .collect();
                                if entries.is_empty() {
                                    continue;
                                }
                                feature_box(column, group, &entries);
                                column.add_space(10.0);
                            }
                        },
                    );
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
        .corner_radius(6)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |section| {
            section.set_width(section.available_width());
            section.spacing_mut().item_spacing.y = 5.0;
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

// ───────────────────────── Gradient ─────────────────────────

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
