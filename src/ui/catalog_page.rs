//! Page « Catalog » : recherche, filtres, pagination et grille de cartes.
use super::{Action, CatalogSelection, game_card, icons, widgets};
use crate::catalog::{ALL_GENRES, Catalog, CatalogSort, InstalledFilter, PLATFORMS};
use crate::images::ImageCache;
use crate::library::Library;
use eframe::egui::{self, Color32, RichText};
use fluent_egui::winui::prelude::*;

/// Largeur cible d'une carte portrait.
const CARD_WIDTH: f32 = 224.0;
/// Espace horizontal et vertical entre deux cartes.
const GRID_GAP: f32 = 12.0;

pub fn show(
    ui: &mut egui::Ui,
    catalog: &mut Catalog,
    selected_store: CatalogSelection,
    library: &Library,
    images: &mut ImageCache,
    out: &mut Vec<Action>,
) {
    render(
        ui,
        catalog,
        selected_store,
        catalog.is_loading(),
        library,
        images,
        out,
    );
}

pub fn show_all(
    ui: &mut egui::Ui,
    combined: &mut Catalog,
    xbox: &Catalog,
    epic: &Catalog,
    library: &Library,
    images: &mut ImageCache,
    out: &mut Vec<Action>,
) {
    combined.sync_combined(xbox, epic);
    render(
        ui,
        combined,
        CatalogSelection::All,
        xbox.is_loading() || epic.is_loading(),
        library,
        images,
        out,
    );
}

fn render(
    ui: &mut egui::Ui,
    catalog: &mut Catalog,
    selected_store: CatalogSelection,
    loading: bool,
    library: &Library,
    images: &mut ImageCache,
    out: &mut Vec<Action>,
) {
    catalog.refresh_visible(library);
    let mut area = egui::ScrollArea::vertical()
        .id_salt(("catalog_scroll", selected_store as u8))
        .max_height(ui.available_height())
        .auto_shrink([false, false]);
    if std::mem::take(&mut catalog.scroll_top) {
        area = area.vertical_scroll_offset(0.0);
    }
    area.show(ui, |ui| {
        ui.winui(|ui| {
            widgets::page_header(
                ui,
                "Discover",
                "Catalog",
                "Browse Xbox Game Pass and Epic Games.",
            );
            store_selector(ui, selected_store, out);
            ui.add_space(10.0);
            toolbar(ui, catalog, selected_store, out);
            genre_filter(ui, catalog);
            let total_pages = catalog.total_pages();
            catalog.page_index = catalog.page_index.min(total_pages - 1);

            widgets::status_line(ui, loading, &catalog.status);
            ui.add_space(8.0);
            grid(ui, catalog, images, selected_store, loading, out);
            ui.add_space(12.0);
            pagination(ui, catalog, total_pages);
            ui.add_space(24.0);
        });
    });
}

fn store_selector(ui: &mut egui::Ui, selected: CatalogSelection, out: &mut Vec<Action>) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        store_tab(ui, CatalogSelection::All, selected, out);
        store_tab(ui, CatalogSelection::Xbox, selected, out);
        store_tab(ui, CatalogSelection::Epic, selected, out);
    });
    // Filet de séparation sous les onglets.
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0, super::theme::BORDER);
    ui.add_space(14.0);
}

/// Onglet de catalogue : texte, avec un trait bleu sous l'onglet actif.
fn store_tab(
    ui: &mut egui::Ui,
    store: CatalogSelection,
    selected: CatalogSelection,
    out: &mut Vec<Action>,
) {
    let (title, logo) = match store {
        CatalogSelection::All => ("Entire catalog", icons::catalog(18.0)),
        CatalogSelection::Xbox => ("Xbox Game Pass", icons::xbox_game_pass(18.0)),
        CatalogSelection::Epic => ("Epic Games", icons::epic_games(18.0)),
    };
    let is_selected = store == selected;
    let font = egui::FontId::proportional(13.0);
    let galley = ui.painter().layout_no_wrap(
        title.to_owned(),
        font.clone(),
        super::theme::TEXT_TITLE,
    );
    let width = galley.size().x + 52.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 42.0), egui::Sense::click());
    let hover = ui
        .ctx()
        .animate_bool_with_time(response.id.with("tab-hover"), response.hovered(), 0.12);
    let active = ui
        .ctx()
        .animate_bool_with_time(response.id.with("tab-active"), is_selected, 0.2);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let text_color = super::theme::mix((138, 146, 160), (245, 247, 250), hover.max(active));
    let painter = ui.painter();
    logo.tint(if is_selected {
        Color32::WHITE
    } else {
        super::theme::TEXT_MUTED
    })
    .paint_at(
        ui,
        egui::Rect::from_min_size(
            egui::pos2(rect.left() + 12.0, rect.center().y - 9.0),
            egui::vec2(18.0, 18.0),
        ),
    );
    painter.text(
        egui::pos2(rect.left() + 38.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        title,
        font,
        text_color,
    );
    if active > 0.0 {
        let bar = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 8.0, rect.bottom() - 2.0),
            egui::pos2(rect.right() - 8.0, rect.bottom()),
        );
        painter.rect_filled(bar, 1, super::theme::ACCENT.gamma_multiply(active));
    }
    if response.clicked() {
        out.push(Action::SelectCatalog(store));
    }
}

fn toolbar(
    ui: &mut egui::Ui,
    catalog: &mut Catalog,
    selected: CatalogSelection,
    out: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        ui.add(
            FluentTextEdit::new(&mut catalog.search).hint_text("Search a title, publisher or genre"),
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
        if widgets::pill_button(ui, "Refresh", None, None, !catalog.is_loading()).clicked() {
            match selected {
                CatalogSelection::Xbox => out.push(Action::RefreshCatalog),
                CatalogSelection::Epic => out.push(Action::RefreshEpicCatalog),
                CatalogSelection::All => {
                    out.push(Action::RefreshCatalog);
                    out.push(Action::RefreshEpicCatalog);
                }
            }
        }
    });
}

fn genre_filter(ui: &mut egui::Ui, catalog: &mut Catalog) {
    widgets::pill_scope(ui, |ui| {
        egui::ComboBox::from_id_salt(("genre_filter", catalog.store as u8))
            .selected_text(format!("Category: {}", catalog.genre_filter))
            .width(220.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut catalog.genre_filter, ALL_GENRES.to_owned(), ALL_GENRES);
                for genre in &catalog.genres {
                    ui.selectable_value(&mut catalog.genre_filter, genre.clone(), genre);
                }
            });
    });
    ui.add_space(6.0);
}

fn pagination(ui: &mut egui::Ui, catalog: &mut Catalog, total_pages: usize) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{} result(s)", catalog.visible.len())).size(11.0));
        ui.add_space(12.0);
        let can_previous = catalog.page_index > 0;
        if widgets::pill_button(
            ui,
            "Previous",
            Some(icons::chevron_left(14.0)),
            None,
            can_previous,
        )
        .clicked()
        {
            catalog.page_index -= 1;
            catalog.scroll_top = true;
        }
        ui.label(format!("Page {} / {}", catalog.page_index + 1, total_pages));
        let can_next = catalog.page_index + 1 < total_pages;
        if widgets::pill_button(ui, "Next", None, Some(icons::chevron_right(14.0)), can_next)
            .clicked()
        {
            catalog.page_index += 1;
            catalog.scroll_top = true;
        }
    });
}

fn grid(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    images: &mut ImageCache,
    selected: CatalogSelection,
    loading: bool,
    out: &mut Vec<Action>,
) {
    if catalog.visible.is_empty() {
        empty_state(ui, catalog, selected, loading, out);
        return;
    }
    let available = ui.available_width();
    let count = ((available / CARD_WIDTH).floor() as usize).clamp(1, 5);
    let games = catalog.page_games();

    // Largeur de carte calculée une fois pour toutes les lignes : toutes les cartes ont
    // exactement la même taille (avant, `ui.columns` pouvait élargir certaines lignes).
    let width = ((available - GRID_GAP * (count - 1) as f32) / count as f32).floor();
    for row in games.chunks(count) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = GRID_GAP;
            for game in row {
                images.request(ui.ctx(), &game.id, &game.image, (480, 480));
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 10.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(width);
                        game_card::show(ui, game, images.get(&game.id), loading, out);
                    },
                );
            }
        });
        ui.add_space(GRID_GAP);
    }
}

fn empty_state(
    ui: &mut egui::Ui,
    catalog: &Catalog,
    selected: CatalogSelection,
    loading: bool,
    out: &mut Vec<Action>,
) {
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        let text = if catalog.games.is_empty() && loading {
            "Loading catalog…"
        } else if catalog.games.is_empty() {
            "The catalog is empty. Click “Refresh”."
        } else {
            "No game matches your search."
        };
        ui.label(RichText::new(text).color(super::theme::TEXT_MUTED));
        if !catalog.games.is_empty() && ui.button("Reset filters").clicked() {
            out.push(match selected {
                CatalogSelection::All => Action::ResetAllCatalogFilters,
                CatalogSelection::Xbox => Action::ResetCatalogFilters,
                CatalogSelection::Epic => Action::ResetEpicCatalogFilters,
            });
        }
    });
}
