//! Icônes SVG embarquées (assets/*.svg).
use super::widgets::icon;
use eframe::egui::Image;

macro_rules! icons {
    ($($name:ident => $file:literal),* $(,)?) => {
        $(
            pub fn $name(size: f32) -> Image<'static> {
                icon(
                    concat!("bytes://", $file),
                    include_bytes!(concat!("../../assets/", $file)),
                    size,
                )
            }
        )*
    };
}

icons! {
    logo => "logo.svg",
    dashboard => "dashboard.svg",
    catalog => "catalog.svg",
    xbox_game_pass => "xbox-game-pass.svg",
    epic_games => "epic-games.svg",
    folder => "folder.svg",
    download => "download.svg",
    settings => "settings.svg",
    chevron_left => "chevron-left.svg",
    chevron_right => "chevron-right.svg",
    storage => "storage.svg",
    players => "players.svg",
    shield => "shield.svg",
    info => "info.svg",
    star => "star.svg",
    calendar => "calendar.svg",
    grid => "grid.svg",
    list => "list.svg",
    play => "play.svg",
    pause => "pause.svg",
    cancel => "cancel.svg",
    check => "check.svg",
    folder_check => "folder-check.svg",
    refresh => "refresh.svg",
    trash => "trash.svg",
}
