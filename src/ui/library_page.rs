//! Page « My Library » : dossiers de jeux installés localement.
use super::{Action, Page, icons, theme, widgets};
use crate::catalog::Catalog;
use crate::images::{self, ImageCache};
use crate::library::{Library, LibraryRow, LibrarySort, LibraryView};
use crate::settings::Settings;
use crate::util::{GIB, open_folder};
use eframe::egui::{self, Color32, RichText};
use fluent_egui::winui::prelude::*;

const COVER_HEIGHT: f32 = 148.0;

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
    let gap = 12.0;
    let min_card_width = 300.0;
    let available = ui.available_width();
    let columns = match view {
        LibraryView::Grid => (((available + gap) / (min_card_width + gap)).floor() as usize).clamp(1, 4),
        LibraryView::List => 1,
    };
    let card_width = if view == LibraryView::Grid {
        ((available - gap * (columns - 1) as f32) / columns as f32).max(1.0)
    } else {
        available
    };

    for chunk in rows.chunks(columns) {
        ui.horizontal_top(|row_ui| {
            row_ui.spacing_mut().item_spacing.x = gap;
            for row in chunk {
                row_ui.allocate_ui_with_layout(
                    egui::vec2(card_width, 1.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |card_ui| entry_card(card_ui, row, view, images, out),
                );
            }
        });
        ui.add_space(gap);
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
    let subtitle = row.entry.path.display().to_string();

    column.winui(|column| {
        egui::Frame::new()
            .fill(theme::BG_CARD)
            .stroke(egui::Stroke::new(1.0, theme::accent_alpha(45)))
            .corner_radius(14)
            .inner_margin(egui::Margin::same(0))
            .show(column, |card| {
            card.set_min_width(card.available_width());
            card.spacing_mut().item_spacing.y = 0.0;
            if grid {
                cover_image(card, cover, title);
            }
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(card, |body| {
                    body.set_min_width(body.available_width());
                    body.add_sized(
                        [body.available_width(), 24.0],
                        egui::Label::new(RichText::new(title).size(16.0).strong()).truncate(),
                    );
                    let publisher = row.game.map(|game| game.publisher.as_str()).filter(|v| !v.is_empty()).unwrap_or("Installed game");
                    body.label(widgets::muted(publisher, 12.0));
                    body.add_space(7.0);
                    body.horizontal(|meta| {
                        meta.add(icons::storage(13.0));
                        meta.label(widgets::muted(format!("{:.1} GB", row.entry.bytes as f64 / GIB), 11.0));
                    });
                    body.add_space(3.0);
                    body.horizontal(|meta| {
                        meta.add(icons::folder(13.0));
                        meta.add(egui::Label::new(widgets::muted(subtitle, 10.0)).truncate());
                    });
                    body.add_space(9.0);
                    body.horizontal_wrapped(|ui| buttons(ui, row, out));
                });
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
