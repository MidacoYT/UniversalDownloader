//! Design system « Connect » : palette sombre charbon, accent bleu, bordures fines.
//! Les noms des constantes sont conservés : toute l'interface reste compatible.
use eframe::egui::Color32;

// ── Accent ─────────────────────────────────────────────
/// Bleu signature (CTA, sélection, progression).
pub const ACCENT: Color32 = Color32::from_rgb(0, 114, 255);
pub const ACCENT_LIGHT: Color32 = Color32::from_rgb(74, 160, 255);
/// Bleu foncé pour l'état pressé / actif.
pub const ACCENT_DEEP: Color32 = Color32::from_rgb(0, 88, 204);

// ── Surfaces ───────────────────────────────────────────
pub const BG_APP: Color32 = Color32::from_rgb(10, 12, 16);
pub const BG_SIDEBAR: Color32 = Color32::from_rgb(14, 17, 23);
pub const BG_CARD: Color32 = Color32::from_rgb(22, 26, 34);
pub const BG_CARD_HOVER: Color32 = Color32::from_rgb(30, 35, 46);
pub const BG_MODAL: Color32 = Color32::from_rgb(18, 21, 28);
pub const BORDER: Color32 = Color32::from_rgb(40, 46, 58);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(58, 66, 82);

// ── Texte ──────────────────────────────────────────────
pub const TEXT_TITLE: Color32 = Color32::from_rgb(245, 247, 250);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(138, 146, 160);
pub const TEXT_SUBTLE: Color32 = Color32::from_rgb(110, 118, 132);

// ── Statuts ────────────────────────────────────────────
pub const GREEN: Color32 = Color32::from_rgb(46, 204, 113);
pub const EMERALD: Color32 = Color32::from_rgb(38, 180, 120);
pub const BLUE: Color32 = Color32::from_rgb(59, 130, 246);
/// Couleur de marque Epic (utilisée seulement pour le badge Epic, jamais en vert).
pub const EPIC_BLUE: Color32 = Color32::from_rgb(0, 120, 242);
pub const GOLD: Color32 = Color32::from_rgb(245, 185, 66);
pub const CYAN: Color32 = Color32::from_rgb(56, 189, 248);
pub const DANGER: Color32 = Color32::from_rgb(200, 52, 52);
pub const DANGER_DARK: Color32 = Color32::from_rgb(150, 36, 36);

/// Accent avec transparence (bordures de survol, halos).
pub fn accent_alpha(alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(0, 114, 255, alpha)
}

/// Interpolation linéaire entre deux couleurs RGB (`t` entre 0 et 1).
pub fn mix(from: (u8, u8, u8), to: (u8, u8, u8), t: f32) -> Color32 {
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t.clamp(0.0, 1.0)) as u8;
    Color32::from_rgb(lerp(from.0, to.0), lerp(from.1, to.1), lerp(from.2, to.2))
}
