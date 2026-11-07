//! Universal Downloader — point d'entrée.
//!
//! Architecture :
//! - `model`     : structures de données (Game, attributs, enums de domaine)
//! - `catalog`   : état + filtrage du catalogue, accès réseau (`catalog::api`)
//! - `library`   : bibliothèque locale (scan, correspondance, désinstallation)
//! - `settings`  : réglages persistants
//! - `images`    : cache de textures chargées en arrière-plan
//! - `ui`        : thème, widgets, pages, modales
//! - `app`       : composition + routage des actions

mod app;
mod catalog;
mod downloads;
mod images;
mod library;
mod model;
mod net;
mod player;
mod settings;
mod ui;
mod updates;
mod util;

use eframe::egui;
use fluent_egui::winui::{self, BackdropKind, FluentTheme, ThemeConfig};
use std::sync::Arc;

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "Montserrat-Bold".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/Montserrat-Bold.ttf"
        ))),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .insert(0, "Montserrat-Bold".to_owned());
    ctx.set_fonts(fonts);
}

fn main() -> eframe::Result<()> {
    let theme = ThemeConfig::new(FluentTheme::Dark)
        .with_accent(ui::theme::ACCENT)
        .with_material_opacity(0.78)
        .with_backdrop(BackdropKind::Mica);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Universal Downloader")
            .with_icon(app_icon())
            .with_inner_size([1280.0, 720.0])
            .with_min_inner_size([760.0, 520.0])
            .with_transparent(true),
        ..Default::default()
    };
    eframe::run_native(
        "Universal Downloader",
        options,
        Box::new(move |cc| {
            winui::install(&cc.egui_ctx, theme);
            install_fonts(&cc.egui_ctx);
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(app::App::new()))
        }),
    )
}

fn app_icon() -> egui::IconData {
    let image = egui_extras::image::load_svg_bytes_with_size(
        include_bytes!("../assets/app-icon.svg"),
        egui::SizeHint::Size {
            width: 256,
            height: 256,
            maintain_aspect_ratio: true,
        },
        &Default::default(),
    )
    .expect("embedded app icon SVG should be valid");
    let rgba = image
        .pixels
        .iter()
        .flat_map(|pixel| pixel.to_srgba_unmultiplied())
        .collect();
    egui::IconData {
        rgba,
        width: image.size[0] as u32,
        height: image.size[1] as u32,
    }
}
