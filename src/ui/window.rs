//! Chrome de fenêtre sans décorations natives : zone de déplacement, boutons
//! réduire / agrandir / fermer, poignées de redimensionnement et liseré extérieur.
//!
//! La fenêtre est créée avec `with_decorations(false)` ; tout ce que Windows faisait
//! pour nous (déplacer, double-clic, redimensionner) est refait ici, en un seul endroit.
use super::theme;
use eframe::egui::{
    self, Color32, CursorIcon, Id, LayerId, Order, PointerButton, Pos2, Rect, Sense, Shape, Stroke,
    StrokeKind, ViewportCommand, vec2,
};
use egui::viewport::ResizeDirection;

/// Dimensions d'un bouton de légende (même gabarit que Windows 11).
pub const CAPTION_WIDTH: f32 = 46.0;
pub const CAPTION_HEIGHT: f32 = 32.0;

/// Épaisseur des bords et taille des coins sensibles au redimensionnement.
const RESIZE_EDGE: f32 = 5.0;
const RESIZE_CORNER: f32 = 10.0;

/// Taille du pictogramme dessiné dans un bouton de légende.
const GLYPH: f32 = 10.0;

fn is_maximized(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().maximized.unwrap_or(false))
}

fn is_fullscreen(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().fullscreen.unwrap_or(false))
}

fn toggle_maximized(ctx: &egui::Context) {
    ctx.send_viewport_cmd(ViewportCommand::Maximized(!is_maximized(ctx)));
}

/// Rend `rect` draggable (déplacement de la fenêtre) et gère le double-clic pour maximiser.
///
/// À appeler **avant** les autres widgets de la barre : egui donne la priorité au dernier
/// widget enregistré, les boutons et onglets passent donc au-dessus de cette zone.
pub fn drag_region(ui: &egui::Ui, rect: Rect) {
    let response = ui.interact(rect, ui.id().with("window-drag"), Sense::click_and_drag());
    if response.double_clicked() {
        toggle_maximized(ui.ctx());
    } else if response.drag_started_by(PointerButton::Primary) {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Caption {
    Minimize,
    Maximize,
    Close,
}

/// Dessine et gère les trois boutons collés en haut à droite de `bar`.
/// Retourne le bord gauche du groupe, pour que la barre place le reste à sa gauche.
pub fn caption_buttons(ui: &mut egui::Ui, bar: Rect) -> f32 {
    let maximized = is_maximized(ui.ctx());
    let mut left = bar.right();

    for caption in [Caption::Close, Caption::Maximize, Caption::Minimize] {
        let rect = Rect::from_min_size(
            Pos2::new(left - CAPTION_WIDTH, bar.top()),
            vec2(CAPTION_WIDTH, CAPTION_HEIGHT),
        );
        left = rect.left();

        let response = ui.interact(rect, ui.id().with(("caption", caption as u8)), Sense::click());
        let hover = ui
            .ctx()
            .animate_bool_with_time(response.id.with("hover"), response.hovered(), 0.1);

        let painter = ui.painter();
        let (fill, glyph) = if caption == Caption::Close {
            (
                theme::DANGER.gamma_multiply(hover),
                theme::mix((138, 146, 160), (255, 255, 255), hover),
            )
        } else {
            (
                Color32::from_white_alpha((16.0 * hover) as u8),
                theme::mix((138, 146, 160), (245, 247, 250), hover),
            )
        };
        painter.rect_filled(rect, 0.0, fill);
        paint_glyph(painter, caption, maximized, rect.center(), Stroke::new(1.0, glyph));

        if response.clicked() {
            let ctx = ui.ctx();
            match caption {
                Caption::Minimize => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
                Caption::Maximize => toggle_maximized(ctx),
                Caption::Close => ctx.send_viewport_cmd(ViewportCommand::Close),
            }
        }
    }
    left
}

fn paint_glyph(painter: &egui::Painter, caption: Caption, maximized: bool, center: Pos2, stroke: Stroke) {
    // Centre aligné sur un demi-pixel logique pour des traits nets.
    let c = Pos2::new(center.x.floor() + 0.5, center.y.floor() + 0.5);
    let half = GLYPH / 2.0;
    match caption {
        Caption::Minimize => {
            painter.line_segment([Pos2::new(c.x - half, c.y), Pos2::new(c.x + half, c.y)], stroke);
        }
        Caption::Maximize if !maximized => {
            painter.rect_stroke(
                Rect::from_center_size(c, vec2(GLYPH, GLYPH)),
                1.0,
                stroke,
                StrokeKind::Inside,
            );
        }
        Caption::Maximize => {
            // Deux fenêtres superposées : celle de devant en entier, celle de derrière
            // réduite à ses deux arêtes visibles.
            let side = GLYPH - 2.0;
            let front = Rect::from_min_size(Pos2::new(c.x - half, c.y - half + 2.0), vec2(side, side));
            let back = Rect::from_min_size(front.min + vec2(2.0, -2.0), vec2(side, side));
            painter.rect_stroke(front, 1.0, stroke, StrokeKind::Inside);
            painter.add(Shape::line(
                vec![
                    Pos2::new(back.left(), front.top()),
                    back.left_top(),
                    back.right_top(),
                    back.right_bottom(),
                    Pos2::new(front.right(), back.bottom()),
                ],
                stroke,
            ));
        }
        Caption::Close => {
            painter.line_segment([Pos2::new(c.x - half, c.y - half), Pos2::new(c.x + half, c.y + half)], stroke);
            painter.line_segment([Pos2::new(c.x - half, c.y + half), Pos2::new(c.x + half, c.y - half)], stroke);
        }
    }
}

/// Poignées invisibles sur le pourtour de `area` pour redimensionner la fenêtre.
/// À appeler en dernier dans la frame, pour passer au-dessus du contenu.
pub fn resize_edges(ui: &egui::Ui, area: Rect) {
    let ctx = ui.ctx();
    if is_maximized(ctx) || is_fullscreen(ctx) {
        return;
    }
    let (e, c) = (RESIZE_EDGE, RESIZE_CORNER);
    let (l, r, t, b) = (area.left(), area.right(), area.top(), area.bottom());
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y1));

    // Les coins sont déclarés après les bords : ils gagnent à l'intersection.
    let zones = [
        (rect(l + c, t, r - c, t + e), ResizeDirection::North, CursorIcon::ResizeVertical),
        (rect(l + c, b - e, r - c, b), ResizeDirection::South, CursorIcon::ResizeVertical),
        (rect(l, t + c, l + e, b - c), ResizeDirection::West, CursorIcon::ResizeHorizontal),
        (rect(r - e, t + c, r, b - c), ResizeDirection::East, CursorIcon::ResizeHorizontal),
        (rect(l, t, l + c, t + c), ResizeDirection::NorthWest, CursorIcon::ResizeNwSe),
        (rect(r - c, t, r, t + c), ResizeDirection::NorthEast, CursorIcon::ResizeNeSw),
        (rect(l, b - c, l + c, b), ResizeDirection::SouthWest, CursorIcon::ResizeNeSw),
        (rect(r - c, b - c, r, b), ResizeDirection::SouthEast, CursorIcon::ResizeNwSe),
    ];
    for (index, (zone, direction, cursor)) in zones.into_iter().enumerate() {
        let response = ui.interact(zone, ui.id().with(("window-resize", index)), Sense::drag());
        if response.hovered() || response.dragged() {
            ctx.set_cursor_icon(cursor);
        }
        if response.drag_started_by(PointerButton::Primary) {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
        }
    }
}

/// Liseré d'un pixel autour de la fenêtre (absent en plein écran / maximisée).
pub fn paint_border(ctx: &egui::Context, area: Rect) {
    if is_maximized(ctx) || is_fullscreen(ctx) {
        return;
    }
    ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("window-border")))
        .rect_stroke(area, 0.0, Stroke::new(1.0, theme::BORDER), StrokeKind::Inside);
}
