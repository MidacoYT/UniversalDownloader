//! Palette et constantes visuelles.
use eframe::egui::Color32;

pub const ACCENT: Color32 = Color32::from_rgb(124, 58, 237);
pub const ACCENT_LIGHT: Color32 = Color32::from_rgb(168, 85, 247);
pub const BG_APP: Color32 = Color32::from_rgb(8, 8, 15);
pub const BG_SIDEBAR: Color32 = Color32::from_rgb(9, 9, 21);
pub const BG_CARD: Color32 = Color32::from_rgb(25, 25, 40);
pub const BG_MODAL: Color32 = Color32::from_rgb(16, 16, 30);
pub const TEXT_TITLE: Color32 = Color32::from_rgb(240, 237, 249);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(148, 148, 168);
pub const TEXT_SUBTLE: Color32 = Color32::from_rgb(155, 151, 172);
pub const GREEN: Color32 = Color32::from_rgb(34, 197, 94);
pub const EMERALD: Color32 = Color32::from_rgb(16, 185, 129);
pub const BLUE: Color32 = Color32::from_rgb(59, 130, 246);
pub const EPIC_BLUE: Color32 = Color32::from_rgb(0, 120, 242);
pub const GOLD: Color32 = Color32::from_rgb(251, 191, 36);
pub const CYAN: Color32 = Color32::from_rgb(6, 182, 212);
pub const DANGER: Color32 = Color32::from_rgb(153, 27, 27);
pub const DANGER_DARK: Color32 = Color32::from_rgb(127, 29, 29);

pub fn accent_alpha(alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(124, 58, 237, alpha)
}

/// Interpolation linéaire entre deux couleurs RGB (`t` entre 0 et 1).
pub fn mix(from: (u8, u8, u8), to: (u8, u8, u8), t: f32) -> Color32 {
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t.clamp(0.0, 1.0)) as u8;
    Color32::from_rgb(lerp(from.0, to.0), lerp(from.1, to.1), lerp(from.2, to.2))
}
