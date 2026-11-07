//! Page « My Library » : dossiers de jeux installés localement.
use super::{Action, Page, icons, theme, widgets};
use crate::catalog::Catalog;
use crate::images::{self, ImageCache};
use crate::library::{Library, LibraryRow, LibrarySort, LibraryView};
use crate::settings::Settings;
use crate::util::{GIB, open_folder};
use eframe::egui::{self, Color32, RichText};
use fluent_egui::winui::prelude::*;

const COVER_HEIGHT: f32 = 112.0;

pub fn show(
    ui: &mut egui::Ui,
    library: &mut Library,
    catalog: &Catalog,
    epic_catalog: &Catalog,
    settings: &mut Settings,
    images: &mut ImageCache,
    out: &mut Vec<Action>,
) {
    widgets::page_header(
        ui,
        "LOCAL COLLECTION",
        "My Library",
        "Local games found in their installation folders.",
    );
    path_bar(ui, library, settings, out);
    widgets::status_line(ui, library.is_scanning(), &library.status);
    filter_bar(ui, library);
    ui.add_space(6.0);

    let view = library.view;
    let scanning = library.is_scanning();
    let library = &*library;
    let rows = library.rows(&[catalog.games.as_slice(), epic_catalog.games.as_slice()]);

    egui::ScrollArea::vertical()
        .id_salt("library_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.winui(|ui| {
                if rows.is_empty() && !scanning {
                    empty_state(ui, library.entries.is_empty());
                }
                cards(ui, &rows, view, images, out);
            });
        });
}

fn path_bar(ui: &mut egui::Ui, library: &Library, settings: &mut Settings, out: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.add(FluentTextEdit::new(&mut settings.library_dir).hint_text("Game folder"));
        if ui.button("Browse…").clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .set_directory(&settings.library_dir)
                .pick_folder()
            {
                settings.library_dir = path.display().to_string();
                out.push(Action::ScanLibrary);
            }
        }
        if ui
            .add_enabled(!library.is_scanning(), FluentButton::accent("Scan"))
            .clicked()
        {
            out.push(Action::ScanLibrary);
        }
    });
}

fn filter_bar(ui: &mut egui::Ui, library: &mut Library) {
    ui.horizontal(|ui| {
        ui.add(FluentTextEdit::new(&mut library.search).hint_text("Filter games or folders"));
        widgets::combo(
            ui,
            "library_sort",
            &mut library.sort,
            LibrarySort::ALL.as_slice(),
            LibrarySort::label,
        );
        let views = [
            (LibraryView::Grid, "Grid", icons::grid(14.0)),
            (LibraryView::List, "List", icons::list(14.0)),
        ];
        for (view, label, image) in views {
            let selected = library.view == view;
            let button = egui::Button::image_and_text(image, label)
                .fill(if selected {
                    theme::accent_alpha(75)
                } else {
                    Color32::TRANSPARENT
                })
                .stroke(egui::Stroke::NONE);
            if ui.add(button).clicked() {
                library.view = view;
            }
        }
    });
}

fn empty_state(ui: &mut egui::Ui, nothing_scanned: bool) {
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        let text = if nothing_scanned {
            "No game folders found. Choose another folder."
        } else {
            "No results for this filter."
        };
        ui.label(RichText::new(text).color(Color32::GRAY));
    });
}

fn cards(
    ui: &mut egui::Ui,
    rows: &[LibraryRow],
    view: LibraryView,
    images: &mut ImageCache,
    out: &mut Vec<Action>,
) {
    let columns = match view {
        LibraryView::Grid => ((ui.available_width() / 300.0).floor() as usize).clamp(1, 4),
        LibraryView::List => 1,
    };
    for chunk in rows.chunks(columns) {
        ui.columns(columns, |column_uis| {
            for (column, row) in column_uis.iter_mut().zip(chunk) {
                entry_card(column, row, view, images, out);
            }
        });
    }
}

fn entry_card(
    column: &mut egui::Ui,
    row: &LibraryRow,
    view: LibraryView,
    images: &mut ImageCache,
    out: &mut Vec<Action>,
) {
    let grid = view == LibraryView::Grid;
    if let (true, Some(game)) = (grid, row.game) {
        images.request(column.ctx(), &game.id, &game.image, (480, 480));
    }
    let cover = row.game.and_then(|g| images.get(&g.id));
    let title = row.title();
    let subtitle = format!(
        "{:.1} GB · {}",
        row.entry.bytes as f64 / GIB,
        row.entry.path.display()
    );

    column.winui(|column| {
        column.card(title, subtitle, |card| {
            if grid {
                cover_image(card, cover, title);
                card.add_space(8.0);
            }
            if let Some(game) = row.game {
                let publisher = if game.publisher.is_empty() {
                    "Xbox"
                } else {
                    &game.publisher
                };
                card.label(format!("Found in catalog · {publisher}"));
            }
            card.horizontal(|ui| buttons(ui, row, out));
        });
    });
}

fn buttons(ui: &mut egui::Ui, row: &LibraryRow, out: &mut Vec<Action>) {
    let entry = row.entry;
    if ui
        .add(egui::Button::image_and_text(icons::play(14.0), "Play"))
        .clicked()
    {
        out.push(Action::PlayGame {
            name: entry.name.clone(),
            path: entry.path.clone(),
        });
    }
    if let Some(game) = row.game {
        if ui.button("View details").clicked() {
            out.push(Action::Navigate(Page::Catalog));
            out.push(Action::OpenGame {
                id: game.id.clone(),
                store: game.store,
            });
        }
    }
    if ui.button("Open folder").clicked() {
        open_folder(&entry.path);
    }
    let uninstall = egui::Button::image(icons::trash(14.0)).fill(theme::DANGER_DARK);
    if ui.add(uninstall).on_hover_text("Uninstall").clicked() {
        out.push(Action::AskUninstall(entry.clone()));
    }
}

fn cover_image(ui: &mut egui::Ui, texture: Option<&egui::TextureHandle>, title: &str) {
    let size = egui::vec2(ui.available_width().max(1.0), COVER_HEIGHT);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let radius = egui::CornerRadius::same(8);
    match texture {
        Some(texture) => images::paint_cover(ui, texture, rect, radius, 1.0),
        None => images::paint_placeholder(ui, rect, radius, title),
    }
}
