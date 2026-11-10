//! Bibliothèque : vitrine plein cadre des jeux installés, filtres par boutique,
//! vue en tuiles ou en tableau, menu contextuel sur chaque jeu.
use super::{Action, Page, icons, theme, widgets};
use crate::catalog::Catalog;
use crate::images::{self, ImageCache};
use crate::library::{Library, LibraryRow, LibrarySort, LibraryView};
use crate::model::GameStore;
use crate::settings::Settings;
use crate::util::{GIB, open_folder};
use eframe::egui::{self, Color32, RichText};

const TILE_MIN_WIDTH: f32 = 230.0;
const TILE_GAP: f32 = 16.0;
const TILE_RADIUS: u8 = 10;
/// Hauteur / largeur des tuiles : un peu plus haut qu'un 16:9 pour une lecture « affiche ».
const TILE_ASPECT: f32 = 0.62;
const TABLE_ROW_H: f32 = 64.0;
const TABLE_THUMB: egui::Vec2 = egui::vec2(96.0, 54.0);

/// Filtre par boutique, conservé d'une frame à l'autre dans la mémoire de l'UI.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum StoreFilter {
    #[default]
    All,
    Xbox,
    Epic,
}

impl StoreFilter {
    const ALL: [Self; 3] = [Self::All, Self::Xbox, Self::Epic];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Xbox => "Xbox",
            Self::Epic => "Epic",
        }
    }

    fn accepts(self, store: &GameStore) -> bool {
        match (self, store) {
            (Self::All, _) => true,
            (Self::Xbox, GameStore::Xbox) => true,
            (Self::Epic, GameStore::Epic) => true,
            _ => false,
        }
    }
}

pub fn show(
    ui: &mut egui::Ui,
    library: &mut Library,
    catalog: &Catalog,
    epic_catalog: &Catalog,
    settings: &mut Settings,
    images: &mut ImageCache,
    out: &mut Vec<Action>,
) {
    let scanning = library.is_scanning();

    header(ui, library, settings, out);
    ui.add_space(18.0);
    stats(ui, library, settings);
    ui.add_space(22.0);

    let filter_id = ui.id().with("library_store_filter");
    let mut store = ui.data_mut(|d| d.get_temp::<StoreFilter>(filter_id).unwrap_or_default());
    toolbar(ui, library, &mut store);
    ui.data_mut(|d| d.insert_temp(filter_id, store));

    status(ui, scanning, &library.status);
    ui.add_space(12.0);

    let view = library.view;
    let library = &*library;
    let mut rows = library.rows(&[catalog.games.as_slice(), epic_catalog.games.as_slice()]);
    rows.retain(|row| row.game.is_some_and(|game| store.accepts(&game.store)));

    egui::ScrollArea::vertical()
        .id_salt("library_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if rows.is_empty() && !scanning {
                empty_state(ui, library.entries.is_empty());
            }
            match view {
                LibraryView::Grid => tiles(ui, &rows, images, out),
                LibraryView::List => table(ui, &rows, images, out),
            }
            ui.add_space(40.0);
        });
}

// ───────────────────────── En-tête et statistiques ─────────────────────────

fn header(ui: &mut egui::Ui, library: &Library, settings: &mut Settings, out: &mut Vec<Action>) {
    ui.add_space(24.0);
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(
                RichText::new("Library")
                    .size(34.0)
                    .strong()
                    .color(theme::TEXT_TITLE),
            );
            ui.label(
                RichText::new("Your games installed on this device")
                    .size(13.0)
                    .color(theme::TEXT_MUTED),
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let refresh = egui::Button::new(RichText::new("Refresh").size(13.0))
                .min_size(egui::vec2(0.0, 36.0))
                .fill(theme::BG_CARD)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .corner_radius(8);
            if ui.add_enabled(!library.is_scanning(), refresh).clicked() {
                out.push(Action::ScanLibrary);
            }
            let browse = egui::Button::new(
                RichText::new("Browse…")
                    .size(13.0)
                    .strong()
                    .color(Color32::WHITE),
            )
            .min_size(egui::vec2(0.0, 36.0))
            .fill(theme::ACCENT)
            .stroke(egui::Stroke::NONE)
            .corner_radius(8);
            if ui.add(browse).clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .set_directory(&settings.library_dir)
                    .pick_folder()
                {
                    settings.library_dir = path.display().to_string();
                    out.push(Action::ScanLibrary);
                }
            }
        });
    });
}

fn stats(ui: &mut egui::Ui, library: &Library, settings: &Settings) {
    let count = library.entries.len().to_string();
    let size = format!("{:.1} GB", library.total_bytes() as f64 / GIB);
    let dir = settings.library_dir.clone();
    let gap = 12.0;
    let width = ((ui.available_width() - gap * 2.0) / 3.0).floor();
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        stat_tile(ui, width, "GAMES", &count, "installed");
        stat_tile(ui, width, "SPACE", &size, "used on disk");
        stat_tile(ui, width, "SOURCE", &dir, "scanned folder");
    });
}

fn stat_tile(ui: &mut egui::Ui, width: f32, caption: &str, value: &str, hint: &str) {
    egui::Frame::new()
        .fill(theme::BG_CARD)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(18, 14))
        .show(ui, |ui| {
            ui.set_width(width - 36.0);
            ui.label(
                RichText::new(caption)
                    .size(10.5)
                    .strong()
                    .color(theme::ACCENT_LIGHT),
            );
            ui.add(
                egui::Label::new(
                    RichText::new(value)
                        .size(22.0)
                        .strong()
                        .color(theme::TEXT_TITLE),
                )
                .truncate(),
            );
            ui.add(
                egui::Label::new(RichText::new(hint).size(11.0).color(theme::TEXT_SUBTLE))
                    .truncate(),
            );
        });
}

// ───────────────────────── Filtres ─────────────────────────

fn toolbar(ui: &mut egui::Ui, library: &mut Library, store: &mut StoreFilter) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        segments(ui, store, &StoreFilter::ALL, StoreFilter::label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            segments(
                ui,
                &mut library.view,
                &[LibraryView::Grid, LibraryView::List],
                view_label,
            );
            segments(ui, &mut library.sort, &LibrarySort::ALL, sort_label);
            ui.add(
                egui::TextEdit::singleline(&mut library.search)
                    .hint_text("Search…")
                    .desired_width(240.0)
                    .margin(egui::vec2(10.0, 7.0)),
            );
        });
    });
}

fn view_label(view: LibraryView) -> &'static str {
    match view {
        LibraryView::Grid => "Grid",
        LibraryView::List => "List",
    }
}

fn sort_label(sort: LibrarySort) -> &'static str {
    match sort {
        LibrarySort::Name => "Name",
        LibrarySort::Size => "Size",
    }
}

/// Groupe de segments : une seule option sélectionnée à la fois.
fn segments<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    current: &mut T,
    options: &[T],
    label: fn(T) -> &'static str,
) {
    egui::Frame::new()
        .fill(theme::BG_SIDEBAR)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(9)
        .inner_margin(egui::Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for &option in options {
                    if segment(ui, label(option), *current == option).clicked() {
                        *current = option;
                    }
                }
            });
        });
}

fn segment(ui: &mut egui::Ui, text: &str, selected: bool) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::FontId::proportional(12.5),
        Color32::PLACEHOLDER,
    );
    let size = egui::vec2(galley.size().x + 28.0, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("segment-hover"), response.hovered(), 0.1);
    let fill = if selected {
        theme::ACCENT
    } else {
        theme::mix((14, 17, 23), (36, 42, 56), hover)
    };
    let text_color = if selected {
        Color32::WHITE
    } else {
        theme::TEXT_MUTED.lerp_to_gamma(theme::TEXT_TITLE, hover)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 7.0, fill);
    let pos = egui::pos2(
        rect.center().x - galley.size().x / 2.0,
        rect.center().y - galley.size().y / 2.0,
    );
    painter.galley(pos, galley, text_color);
    response
}

fn status(ui: &mut egui::Ui, busy: bool, text: &str) {
    if !busy && text.is_empty() {
        return;
    }
    ui.horizontal(|ui| {
        if busy {
            ui.spinner();
        }
        ui.label(RichText::new(text).size(11.0).color(theme::TEXT_SUBTLE));
    });
}

// ───────────────────────── Tuiles ─────────────────────────

fn tiles(ui: &mut egui::Ui, rows: &[LibraryRow], images: &mut ImageCache, out: &mut Vec<Action>) {
    let available = ui.available_width();
    let columns =
        (((available + TILE_GAP) / (TILE_MIN_WIDTH + TILE_GAP)).floor() as usize).clamp(1, 6);
    let width = ((available - TILE_GAP * (columns - 1) as f32) / columns as f32).floor();

    for row in rows.iter() {
        if let Some(game) = row.game {
            images.request(ui.ctx(), &game.id, &game.image, (480, 270));
        }
    }
    for chunk in rows.chunks(columns) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = TILE_GAP;
            for row in chunk {
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 1.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(width);
                        tile(ui, row, images, out);
                    },
                );
            }
        });
        ui.add_space(TILE_GAP);
    }
}

fn tile(ui: &mut egui::Ui, row: &LibraryRow, images: &mut ImageCache, out: &mut Vec<Action>) {
    let width = ui.available_width();
    let height = width * TILE_ASPECT;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("tile-hover"), response.hovered(), 0.18);
    let radius = egui::CornerRadius::same(TILE_RADIUS);
    let painter = ui.painter().clone();

    match row.game.and_then(|game| images.get(&game.id)) {
        Some(texture) => images::paint_cover(ui, texture, rect, radius, 1.0 + 0.04 * hover),
        None => images::paint_placeholder(ui, rect, radius, row.title()),
    }
    paint_fade(&painter, rect, hover);

    let border = theme::BORDER.lerp_to_gamma(theme::ACCENT, hover);
    painter.rect_stroke(
        rect,
        radius,
        egui::Stroke::new(1.0, border),
        egui::StrokeKind::Inside,
    );

    if let Some(game) = row.game {
        store_badge(&painter, rect, &game.store);
    }

    // Bouton « Jouer » rond, toujours visible, qui grossit au survol.
    let pad = 14.0;
    let play_center = egui::pos2(rect.right() - pad - 18.0, rect.bottom() - pad - 18.0);
    let play = ui.interact(
        egui::Rect::from_center_size(play_center, egui::vec2(40.0, 40.0)),
        response.id.with("play"),
        egui::Sense::click(),
    );
    let play_r = 18.0 + 2.0 * play.hovered() as u8 as f32;
    painter.circle_filled(play_center, play_r, theme::ACCENT);
    let s = 8.0;
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(play_center.x - s * 0.6, play_center.y - s),
            egui::pos2(play_center.x - s * 0.6, play_center.y + s),
            egui::pos2(play_center.x + s, play_center.y),
        ],
        Color32::WHITE,
        egui::Stroke::NONE,
    ));

    // Titre et éditeur, en bas à gauche (laisse la place au bouton).
    let text_width = width - pad * 2.0 - 48.0;
    let meta_y = rect.bottom() - pad - 14.0;
    let title_y = meta_y - 20.0;
    widgets::paint_truncated(
        &painter,
        egui::pos2(rect.left() + pad, title_y),
        row.title(),
        egui::FontId::proportional(15.0),
        theme::TEXT_TITLE,
        text_width,
    );
    widgets::paint_truncated(
        &painter,
        egui::pos2(rect.left() + pad, meta_y),
        &meta_line(row),
        egui::FontId::proportional(11.0),
        Color32::from_gray(190),
        text_width,
    );

    if play.clicked() {
        push_play(out, row);
    } else if response.clicked() {
        push_details(out, row);
    }
    if response.hovered() || play.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.context_menu(|ui| menu_items(ui, row, out));
}

/// Dégradé sombre en bas de la tuile, plus marqué au survol.
fn paint_fade(painter: &egui::Painter, rect: egui::Rect, hover: f32) {
    let top = rect.bottom() - rect.height() * 0.62;
    let bottom = rect.bottom();
    let clear = Color32::from_black_alpha(0);
    let dark = Color32::from_black_alpha((200.0 + 30.0 * hover) as u8);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(egui::pos2(rect.left(), top), clear);
    mesh.colored_vertex(egui::pos2(rect.right(), top), clear);
    mesh.colored_vertex(egui::pos2(rect.right(), bottom), dark);
    mesh.colored_vertex(egui::pos2(rect.left(), bottom), dark);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

fn store_badge(painter: &egui::Painter, rect: egui::Rect, store: &GameStore) {
    let (label, color) = match store {
        GameStore::Xbox => ("XBOX", theme::GREEN),
        GameStore::Epic => ("EPIC", theme::EPIC_BLUE),
    };
    let text_width = painter
        .layout_no_wrap(label.to_owned(), egui::FontId::proportional(9.5), color)
        .size()
        .x;
    let badge_size = egui::vec2(text_width + 16.0, 19.0);
    let badge = egui::Rect::from_min_size(
        egui::pos2(rect.right() - badge_size.x - 10.0, rect.top() + 10.0),
        badge_size,
    );
    painter.rect_filled(badge, 4.0, Color32::from_rgba_unmultiplied(8, 10, 14, 220));
    painter.text(
        badge.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(9.5),
        color,
    );
}

// ───────────────────────── Tableau ─────────────────────────

fn table(ui: &mut egui::Ui, rows: &[LibraryRow], images: &mut ImageCache, out: &mut Vec<Action>) {
    const PUBLISHER_W: f32 = 200.0;
    const SIZE_W: f32 = 90.0;
    const PATH_W: f32 = 260.0;
    const ACTIONS_W: f32 = 200.0;

    let width = ui.available_width();
    let title_w = (width - PUBLISHER_W - SIZE_W - PATH_W - ACTIONS_W - 20.0).max(220.0);

    // En-tête de colonnes.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        cell(ui, title_w, 22.0, |ui| head(ui, "GAME"));
        cell(ui, PUBLISHER_W, 22.0, |ui| head(ui, "PUBLISHER"));
        cell(ui, SIZE_W, 22.0, |ui| head(ui, "SIZE"));
        cell(ui, PATH_W, 22.0, |ui| head(ui, "FOLDER"));
        cell(ui, ACTIONS_W, 22.0, |ui| head(ui, ""));
    });
    ui.add_space(4.0);

    for (index, row) in rows.iter().enumerate() {
        if let Some(game) = row.game {
            images.request(ui.ctx(), &game.id, &game.image, (480, 270));
        }
        let fill = if index % 2 == 0 {
            theme::BG_CARD
        } else {
            theme::BG_APP
        };
        egui::Frame::new()
            .fill(fill)
            .corner_radius(6)
            .inner_margin(egui::Margin::symmetric(10, 0))
            .show(ui, |ui| {
                ui.set_width(width - 20.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    cell(ui, title_w, TABLE_ROW_H, |ui| {
                        let (thumb, _) = ui.allocate_exact_size(TABLE_THUMB, egui::Sense::hover());
                        let radius = egui::CornerRadius::same(4);
                        match row.game.and_then(|game| images.get(&game.id)) {
                            Some(texture) => images::paint_cover(ui, texture, thumb, radius, 1.0),
                            None => images::paint_placeholder(ui, thumb, radius, row.title()),
                        }
                        ui.add_space(12.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.add(
                                egui::Label::new(RichText::new(row.title()).size(14.0).strong())
                                    .truncate(),
                            );
                            ui.add(
                                egui::Label::new(widgets::muted(store_name(row), 11.0)).truncate(),
                            );
                        });
                    });
                    cell(ui, PUBLISHER_W, TABLE_ROW_H, |ui| {
                        let publisher = row
                            .game
                            .map(|game| game.publisher.as_str())
                            .filter(|v| !v.is_empty())
                            .unwrap_or("—");
                        ui.add(egui::Label::new(widgets::muted(publisher, 12.0)).truncate());
                    });
                    cell(ui, SIZE_W, TABLE_ROW_H, |ui| {
                        ui.label(
                            RichText::new(format!("{:.1} GB", row.entry.bytes as f64 / GIB))
                                .size(12.5)
                                .color(theme::TEXT_TITLE),
                        );
                    });
                    cell(ui, PATH_W, TABLE_ROW_H, |ui| {
                        ui.add(
                            egui::Label::new(widgets::muted(
                                row.entry.path.display().to_string(),
                                10.5,
                            ))
                            .truncate(),
                        );
                    });
                    cell(ui, ACTIONS_W, TABLE_ROW_H, |ui| row_actions(ui, row, out));
                });
            });
        ui.add_space(4.0);
    }
}

/// Cellule de largeur fixe pour aligner les colonnes du tableau.
fn cell(ui: &mut egui::Ui, width: f32, height: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, height),
        egui::Layout::left_to_right(egui::Align::Center),
        add,
    );
}

fn head(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(10.0)
            .strong()
            .color(theme::TEXT_SUBTLE),
    );
}

fn row_actions(ui: &mut egui::Ui, row: &LibraryRow, out: &mut Vec<Action>) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let trash = egui::Button::image(icons::trash(14.0).tint(theme::DANGER))
            .min_size(egui::vec2(30.0, 30.0))
            .fill(Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .corner_radius(6);
        if ui.add(trash).on_hover_text("Uninstall").clicked() {
            out.push(Action::AskUninstall(row.entry.clone()));
        }
        let folder = egui::Button::image(icons::folder(15.0).tint(theme::TEXT_MUTED))
            .min_size(egui::vec2(30.0, 30.0))
            .fill(Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .corner_radius(6);
        if ui.add(folder).on_hover_text("Open folder").clicked() {
            open_folder(&row.entry.path);
        }
        if row.game.is_some() {
            let info = egui::Button::image(icons::info(15.0).tint(theme::TEXT_MUTED))
                .min_size(egui::vec2(30.0, 30.0))
                .fill(Color32::TRANSPARENT)
                .stroke(egui::Stroke::NONE)
                .corner_radius(6);
            if ui.add(info).on_hover_text("View details").clicked() {
                push_details(out, row);
            }
        }
        let play = egui::Button::new(
            RichText::new("Play")
                .size(12.5)
                .strong()
                .color(Color32::WHITE),
        )
        .min_size(egui::vec2(80.0, 30.0))
        .fill(theme::ACCENT)
        .stroke(egui::Stroke::NONE)
        .corner_radius(6);
        if ui.add(play).clicked() {
            push_play(out, row);
        }
    });
}

// ───────────────────────── Commun ─────────────────────────

fn menu_items(ui: &mut egui::Ui, row: &LibraryRow, out: &mut Vec<Action>) {
    if ui.button("Play").clicked() {
        push_play(out, row);
    }
    if row.game.is_some() && ui.button("View details").clicked() {
        push_details(out, row);
    }
    if ui.button("Open folder").clicked() {
        open_folder(&row.entry.path);
    }
    ui.separator();
    if ui.button("Uninstall").clicked() {
        out.push(Action::AskUninstall(row.entry.clone()));
    }
}

fn meta_line(row: &LibraryRow) -> String {
    let size = format!("{:.1} GB", row.entry.bytes as f64 / GIB);
    match row
        .game
        .map(|game| game.publisher.as_str())
        .filter(|v| !v.is_empty())
    {
        Some(publisher) => format!("{publisher}  ·  {size}"),
        None => format!("Installed game  ·  {size}"),
    }
}

fn store_name(row: &LibraryRow) -> &'static str {
    match row.game.map(|game| &game.store) {
        Some(GameStore::Epic) => "Epic Games",
        Some(GameStore::Xbox) => "Xbox Game Pass",
        None => "Installed locally",
    }
}

fn push_play(out: &mut Vec<Action>, row: &LibraryRow) {
    out.push(Action::PlayGame {
        name: row.entry.name.clone(),
        path: row.entry.path.clone(),
    });
}

fn push_details(out: &mut Vec<Action>, row: &LibraryRow) {
    if let Some(game) = row.game {
        out.push(Action::Navigate(Page::Catalog));
        out.push(Action::OpenGame {
            id: game.id.clone(),
            store: game.store,
        });
    }
}

fn empty_state(ui: &mut egui::Ui, nothing_scanned: bool) {
    ui.add_space(70.0);
    ui.vertical_centered(|ui| {
        let (title, text) = if nothing_scanned {
            (
                "No games detected",
                "Choose another folder or run the scan again once the catalogs are loaded.",
            )
        } else {
            (
                "No results",
                "No compatible game in this folder matches your search or the selected filter.",
            )
        };
        ui.label(
            RichText::new(title)
                .size(18.0)
                .strong()
                .color(theme::TEXT_TITLE),
        );
        ui.add_space(6.0);
        ui.label(RichText::new(text).color(theme::TEXT_MUTED));
    });
}
