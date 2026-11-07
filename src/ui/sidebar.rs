//! Barre latérale : logo, navigation, progression du téléchargement en cours.
//! Tout est aligné sur une même marge gauche (INSET) : logo, titre de section, icônes, pied de page.
use super::{Action, Page, icons, theme};
use eframe::egui::{self, Color32, RichText};

const INSET: f32 = 12.0;
const ICON_SIZE: f32 = 17.0;
const ICON_GAP: f32 = 12.0;
const ITEM_HEIGHT: f32 = 38.0;

pub fn show(ui: &mut egui::Ui, current: Page, out: &mut Vec<Action>) {
    ui.spacing_mut().item_spacing.y = 4.0;
    ui.add_space(24.0);
    brand(ui);
    ui.add_space(28.0);
    section_label(ui, "WORKSPACE");
    ui.add_space(4.0);

    let items = [
        (Page::Home, "Home", icons::dashboard(ICON_SIZE)),
        (Page::Catalog, "Catalog", icons::catalog(ICON_SIZE)),
        (Page::EpicCatalog, "Epic Games", icons::catalog(ICON_SIZE)),
        (Page::Library, "My Library", icons::folder(ICON_SIZE)),
        (Page::Downloads, "Downloads", icons::download(ICON_SIZE)),
        (Page::Updates, "Updates", icons::calendar(ICON_SIZE)),
        (Page::Settings, "Settings", icons::settings(ICON_SIZE)),
    ];
    for (page, label, image) in items {
        if nav_item(ui, label, image, current == page).clicked() {
            out.push(Action::Navigate(page));
        }
    }
    footer(ui);
}

fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(INSET);
        ui.label(RichText::new(text).size(10.0).color(Color32::GRAY));
    });
}

fn brand(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.add_space(INSET);
        ui.add(icons::logo(38.0));
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(RichText::new("UNIVERSAL").size(14.0).strong());
            ui.label(RichText::new("DOWNLOADER").size(10.0).color(Color32::GRAY));
        });
    });
}

/// Entrée de navigation dessinée à la main : icône et texte sur des colonnes fixes.
fn nav_item(
    ui: &mut egui::Ui,
    label: &str,
    image: egui::Image<'static>,
    selected: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ITEM_HEIGHT),
        egui::Sense::click(),
    );
    let selection = ui
        .ctx()
        .animate_bool_with_time(response.id.with("selected"), selected, 0.25);
    let hover =
        ui.ctx()
            .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.15);

    let painter = ui.painter();
    let fill = if selection > 0.0 {
        theme::accent_alpha((selection * 38.0) as u8)
    } else {
        Color32::from_white_alpha((hover * 10.0) as u8)
    };
    painter.rect_filled(rect, 8, fill);

    if selection > 0.0 {
        let bar = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 2.0, rect.center().y),
            egui::vec2(3.0, rect.height() * 0.6 * selection),
        );
        painter.rect_filled(bar, 2, theme::ACCENT_LIGHT);
    }

    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + INSET, rect.center().y - ICON_SIZE / 2.0),
        egui::vec2(ICON_SIZE, ICON_SIZE),
    );
    image.paint_at(ui, icon_rect);

    painter.text(
        egui::pos2(icon_rect.right() + ICON_GAP, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.0),
        if selected {
            Color32::WHITE
        } else {
            Color32::from_gray(205)
        },
    );
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn footer(ui: &mut egui::Ui) {
    ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(INSET as i8, 0))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add_space(14.0);
                ui.label(
                    RichText::new("FLUENT DESIGN · RUST")
                        .size(9.0)
                        .color(Color32::GRAY),
                );
                ui.label(
                    RichText::new("Xbox catalog")
                        .size(10.0)
                        .color(Color32::from_gray(145)),
                );
            });
    });
}
