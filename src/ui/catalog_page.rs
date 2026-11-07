//! Page « Catalog » : recherche, filtres, pagination et grille de cartes.
use super::{Action, game_card, icons, widgets};
use crate::catalog::{ALL_GENRES, Catalog, CatalogSort, InstalledFilter, PLATFORMS};
use crate::images::ImageCache;
use crate::library::Library;
use crate::model::GameStore;
use eframe::egui::{self, Color32, RichText};
use fluent_egui::winui::prelude::*;

/// Largeur cible d'une carte portrait.
const CARD_WIDTH: f32 = 224.0;

pub fn show(
    ui: &mut egui::Ui,
    catalog: &mut Catalog,
    library: &Library,
    images: &mut ImageCache,
    out: &mut Vec<Action>,
) {
    widgets::page_header(
        ui,
        "DISCOVER",
        if catalog.store == GameStore::Xbox {
            "Xbox catalog"
        } else {
            "Epic Games catalog"
        },
        if catalog.store == GameStore::Xbox {
            "Browse Xbox games and quickly find their details."
        } else {
            "Browse games with a local Epic BuildPatch manifest."
        },
    );
    toolbar(ui, catalog, out);
    genre_chips(ui, catalog);

    catalog.refresh_visible(library);
    let total_pages = catalog.total_pages();
    catalog.page_index = catalog.page_index.min(total_pages - 1);

    widgets::status_line(ui, catalog.is_loading(), &catalog.status);
    pagination(ui, catalog, total_pages);
    ui.add_space(6.0);

    let mut area = egui::ScrollArea::vertical()
        .id_salt("catalog_scroll")
        .auto_shrink([false, false]);
    if std::mem::take(&mut catalog.scroll_top) {
        area = area.vertical_scroll_offset(0.0);
    }
    let catalog = &*catalog;
    area.show(ui, |ui| ui.winui(|ui| grid(ui, catalog, images, out)));
}

fn toolbar(ui: &mut egui::Ui, catalog: &mut Catalog, out: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.add(
            FluentTextEdit::new(&mut catalog.search).hint_text("Search title, studio, or genre"),
        );
        widgets::combo(
            ui,
            "platform_filter",
            &mut catalog.platform_filter,
            PLATFORMS.as_slice(),
            widgets::identity,
        );
        widgets::combo(
            ui,
            "installed_filter",
            &mut catalog.installed_filter,
            InstalledFilter::ALL.as_slice(),
            InstalledFilter::label,
        );
        widgets::combo(
            ui,
            "catalog_sort",
            &mut catalog.sort,
            CatalogSort::ALL.as_slice(),
            CatalogSort::label,
        );
        if ui
            .add_enabled(!catalog.is_loading(), egui::Button::new("Refresh"))
            .clicked()
        {
            out.push(match catalog.store {
                GameStore::Xbox => Action::RefreshCatalog,
                GameStore::Epic => Action::RefreshEpicCatalog,
            });
        }
    });
}

fn genre_chips(ui: &mut egui::Ui, catalog: &mut Catalog) {
    if catalog.genres.is_empty() {
        return;
    }
    let mut picked = None;
    ui.horizontal_wrapped(|ui| {
        let genres = std::iter::once(ALL_GENRES).chain(catalog.genres.iter().map(String::as_str));
        for genre in genres {
            let selected = catalog.genre_filter.eq_ignore_ascii_case(genre);
            if widgets::chip(ui, genre, selected).clicked() {
                picked = Some(genre.to_owned());
            }
        }
    });
    if let Some(genre) = picked {
        catalog.genre_filter = genre;
    }
}

fn pagination(ui: &mut egui::Ui, catalog: &mut Catalog, total_pages: usize) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{} result(s)", catalog.visible.len())).size(11.0));
        ui.add_space(12.0);
        let previous = egui::Button::image_and_text(icons::chevron_left(14.0), "Previous");
        if ui.add_enabled(catalog.page_index > 0, previous).clicked() {
            catalog.page_index -= 1;
            catalog.scroll_top = true;
        }
        ui.label(format!("Page {} / {}", catalog.page_index + 1, total_pages));
        let next = egui::Button::image_and_text(icons::chevron_right(14.0), "Next");
        let can_next = catalog.page_index + 1 < total_pages;
        if ui.add_enabled(can_next, next).clicked() {
            catalog.page_index += 1;
            catalog.scroll_top = true;
        }
    });
}

fn grid(ui: &mut egui::Ui, catalog: &Catalog, images: &mut ImageCache, out: &mut Vec<Action>) {
    if catalog.visible.is_empty() {
        empty_state(ui, catalog, out);
        return;
    }
    let loading = catalog.is_loading();
    let count = ((ui.available_width() / CARD_WIDTH).floor() as usize).clamp(1, 5);
    let games = catalog.page_games();

    for row in games.chunks(count) {
        ui.columns(count, |columns| {
            for (column, game) in columns.iter_mut().zip(row) {
                images.request(column.ctx(), &game.id, &game.image, (480, 480));
                game_card::show(column, game, images.get(&game.id), loading, out);
            }
        });
        ui.add_space(8.0);
    }
}

fn empty_state(ui: &mut egui::Ui, catalog: &Catalog, out: &mut Vec<Action>) {
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        let text = if catalog.games.is_empty() && catalog.is_loading() {
            "Loading catalog…"
        } else if catalog.games.is_empty() {
            "The catalog is empty. Click “Refresh”."
        } else {
            "No games match your search."
        };
        ui.label(RichText::new(text).color(Color32::GRAY));
        if !catalog.games.is_empty() && ui.button("Reset filters").clicked() {
            out.push(match catalog.store {
                GameStore::Xbox => Action::ResetCatalogFilters,
                GameStore::Epic => Action::ResetEpicCatalogFilters,
            });
        }
    });
}
