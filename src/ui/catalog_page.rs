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
    selected_store: GameStore,
    library: &Library,
    images: &mut ImageCache,
    out: &mut Vec<Action>,
) {
    catalog.refresh_visible(library);
    let mut area = egui::ScrollArea::vertical()
        .id_salt(("catalog_scroll", catalog.store as u8))
        .auto_shrink([false, false]);
    if std::mem::take(&mut catalog.scroll_top) {
        area = area.vertical_scroll_offset(0.0);
    }
    area.show(ui, |ui| {
        ui.winui(|ui| {
            widgets::page_header(
                ui,
                "DISCOVER",
                "Game catalog",
                "Browse Xbox Game Pass and Epic Games.",
            );
            toolbar(ui, catalog, out);
            genre_filter(ui, catalog);
            let total_pages = catalog.total_pages();
            catalog.page_index = catalog.page_index.min(total_pages - 1);

            widgets::status_line(ui, catalog.is_loading(), &catalog.status);
            ui.add_space(8.0);
            grid(ui, catalog, images, out);
            ui.add_space(12.0);
            pagination(ui, catalog, total_pages);
            ui.add_space(10.0);
            ui.separator();
            store_selector(ui, selected_store, out);
        });
    });
}

fn store_selector(ui: &mut egui::Ui, selected: GameStore, out: &mut Vec<Action>) {
    ui.add_space(8.0);
    ui.vertical_centered(|ui| {
        ui.label(widgets::muted("Switch catalog", 10.0));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space((ui.available_width() - 392.0).max(0.0) / 2.0);
            store_button(ui, GameStore::Xbox, selected, out);
            ui.add_space(8.0);
            store_button(ui, GameStore::Epic, selected, out);
        });
    });
}

fn store_button(ui: &mut egui::Ui, store: GameStore, selected: GameStore, out: &mut Vec<Action>) {
    let (title, logo) = match store {
        GameStore::Xbox => ("Xbox Game Pass", icons::xbox_game_pass(24.0)),
        GameStore::Epic => ("Epic Games", icons::epic_games(24.0)),
    };
    let is_selected = store == selected;
    let fill = if is_selected {
        super::theme::accent_alpha(55)
    } else {
        super::theme::BG_CARD
    };
    let stroke = if is_selected {
        super::theme::ACCENT_LIGHT
    } else {
        super::theme::accent_alpha(75)
    };
    let button = egui::Button::image_and_text(logo, title)
        .fill(fill)
        .stroke(egui::Stroke::new(
            if is_selected { 1.5 } else { 1.0 },
            stroke,
        ))
        .corner_radius(9);
    if ui.add_sized(egui::vec2(190.0, 42.0), button).clicked() {
        out.push(Action::SelectCatalog(store));
    }
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

fn genre_filter(ui: &mut egui::Ui, catalog: &mut Catalog) {
    egui::ComboBox::from_id_salt(("genre_filter", catalog.store as u8))
        .selected_text(format!("Category: {}", catalog.genre_filter))
        .width(220.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut catalog.genre_filter, ALL_GENRES.to_owned(), ALL_GENRES);
            for genre in &catalog.genres {
                ui.selectable_value(&mut catalog.genre_filter, genre.clone(), genre);
            }
        });
    ui.add_space(6.0);
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
