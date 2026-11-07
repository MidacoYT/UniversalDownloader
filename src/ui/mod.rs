//! Couche interface : thème, widgets partagés, pages et fenêtres modales.
//!
//! Les pages sont de simples fonctions `show(...)` qui reçoivent uniquement l'état dont elles
//! ont besoin et émettent des [`Action`] ; c'est `App` qui applique ces actions.

pub mod catalog_page;
pub mod downloads_page;
pub mod game_card;
pub mod game_modal;
pub mod home;
pub mod icons;
pub mod library_page;
pub mod settings_page;
pub mod sidebar;
pub mod theme;
pub mod trailer_overlay;
pub mod uninstall_modal;
pub mod updates_page;
pub mod widgets;

use crate::library::LibraryEntry;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Home,
    Catalog,
    Library,
    Downloads,
    Updates,
    Settings,
}

/// Intentions émises par l'interface, traitées par `App::apply`.
pub enum Action {
    Navigate(Page),
    SelectCatalog(crate::model::GameStore),
    OpenGame {
        id: String,
        store: crate::model::GameStore,
    },
    PrepareDownload {
        id: String,
        name: String,
        store: crate::model::GameStore,
    },
    CloseGame,
    PlayTrailer {
        title: String,
        url: String,
    },
    CloseTrailer,
    RefreshCatalog,
    RefreshEpicCatalog,
    ResetCatalogFilters,
    ResetEpicCatalogFilters,
    ScanLibrary,
    PlayGame {
        name: String,
        path: PathBuf,
    },
    AskUninstall(LibraryEntry),
    ConfirmUninstall,
    DismissUninstall,
}
