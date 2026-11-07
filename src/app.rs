//! Composition de l'application : état global, routage des pages, application des actions.
use crate::catalog::Catalog;
use crate::downloads::DownloadManager;
use crate::images::ImageCache;
use crate::library::{self, Library, LibraryEntry};
use crate::model::GameStore;
use crate::player::TrailerPlayer;
use crate::settings::Settings;
use crate::ui::game_modal::ModalState;
use crate::ui::{
    Action, Page, catalog_page, downloads_page, game_modal, home, library_page, settings_page,
    sidebar, theme, trailer_overlay, uninstall_modal, updates_page,
};
use crate::updates::UpdateCheck;
use eframe::egui;
use fluent_egui::winui::prelude::*;
use fluent_egui::winui::{self, BackdropKind, FluentTheme};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

const SIDEBAR_W: f32 = 232.0;
const MAIN_MARGIN: f32 = 28.0;

pub struct App {
    page: Page,
    catalog: Catalog,
    epic_catalog: Catalog,
    epic_stats_tx: Sender<(String, Result<(Option<u8>, Option<u8>), String>)>,
    epic_stats_rx: Receiver<(String, Result<(Option<u8>, Option<u8>), String>)>,
    selected_catalog: GameStore,
    library: Library,
    settings: Settings,
    updates: UpdateCheck,
    downloads: DownloadManager,
    download_name: String,
    images: ImageCache,
    selected: Option<(GameStore, String)>,
    modal: ModalState,
    pending_uninstall: Option<LibraryEntry>,
    trailer: TrailerPlayer,
    backdrop_applied: bool,
}

impl App {
    pub fn new() -> Self {
        let (epic_stats_tx, epic_stats_rx) = mpsc::channel();
        let mut app = Self {
            page: Page::Home,
            catalog: Catalog::new(GameStore::Xbox),
            epic_catalog: Catalog::new(GameStore::Epic),
            epic_stats_tx,
            epic_stats_rx,
            selected_catalog: GameStore::Xbox,
            library: Library::new(),
            settings: Settings::load(),
            updates: UpdateCheck::new(),
            downloads: DownloadManager::new(),
            download_name: String::new(),
            images: ImageCache::new(),
            selected: None,
            modal: ModalState::default(),
            pending_uninstall: None,
            trailer: TrailerPlayer::new(),
            backdrop_applied: false,
        };
        app.catalog.refresh();
        app.epic_catalog.refresh();
        app.scan_library();
        app
    }

    // ───────────────────────── Logique ─────────────────────────

    fn scan_library(&mut self) {
        self.settings.persist();
        self.library.scan(&self.settings.library_dir);
    }

    fn rematch_library(&mut self) {
        let games: Vec<_> = self
            .catalog
            .games
            .iter()
            .chain(&self.epic_catalog.games)
            .cloned()
            .collect();
        self.library.rematch(&games);
        self.catalog.mark_dirty();
        self.epic_catalog.mark_dirty();
    }

    /// Intègre les résultats des threads d'arrière-plan.
    fn poll(&mut self, ctx: &egui::Context) {
        self.images.poll(ctx);
        let catalog_changed = self.catalog.poll();
        let epic_catalog_changed = self.epic_catalog.poll();
        let epic_stats_updates: Vec<_> = self.epic_stats_rx.try_iter().collect();
        let epic_stats_changed = !epic_stats_updates.is_empty();
        for (id, result) in epic_stats_updates {
            if let Some(game) = self
                .epic_catalog
                .games
                .iter_mut()
                .find(|game| game.id == id)
            {
                game.epic_stats_loaded = true;
                if let Ok((score, recommend)) = result {
                    game.critic_score = score;
                    game.critic_recommend = recommend;
                }
            }
        }
        let library_changed = self.library.poll();
        self.updates.poll();
        let completed_before = self
            .downloads
            .entries
            .iter()
            .filter(|entry| entry.status == crate::downloads::DownloadStatus::Completed)
            .count();
        self.downloads.poll();
        let completed_after = self
            .downloads
            .entries
            .iter()
            .filter(|entry| entry.status == crate::downloads::DownloadStatus::Completed)
            .count();
        if completed_after > completed_before {
            self.scan_library();
        }
        if catalog_changed || epic_catalog_changed || library_changed {
            self.rematch_library();
        }
        if self.catalog.is_loading()
            || self.epic_catalog.is_loading()
            || epic_catalog_changed
            || epic_stats_changed
            || self.library.is_scanning()
            || self.updates.is_loading()
            || self.downloads.is_active()
        {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Navigate(page) => {
                self.page = page;
                if page == Page::Catalog {
                    let catalog = match self.selected_catalog {
                        GameStore::Xbox => &mut self.catalog,
                        GameStore::Epic => &mut self.epic_catalog,
                    };
                    if catalog.games.is_empty() && !catalog.is_loading() {
                        catalog.refresh();
                    }
                }
            }
            Action::SelectCatalog(store) => {
                self.selected_catalog = store;
                let catalog = match store {
                    GameStore::Xbox => &mut self.catalog,
                    GameStore::Epic => &mut self.epic_catalog,
                };
                catalog.scroll_top = true;
                if catalog.games.is_empty() && !catalog.is_loading() {
                    catalog.refresh();
                }
            }
            Action::OpenGame { id, store } => {
                self.modal = ModalState::default();
                if store == GameStore::Epic {
                    let request = self
                        .epic_catalog
                        .games
                        .iter_mut()
                        .find(|game| game.id == id)
                        .and_then(|game| {
                            if game.egdata_offer_id.is_empty() || game.epic_stats_requested {
                                None
                            } else {
                                game.epic_stats_requested = true;
                                Some((game.id.clone(), game.egdata_offer_id.clone()))
                            }
                        });
                    if let Some((game_id, offer_id)) = request {
                        let tx = self.epic_stats_tx.clone();
                        std::thread::spawn(move || {
                            let result = crate::catalog::api::fetch_epic_critic_stats(&offer_id);
                            let _ = tx.send((game_id, result));
                        });
                    }
                }
                self.selected = Some((store, id));
            }
            Action::PrepareDownload { id, name, store } => {
                self.trailer.close();
                self.selected = None;
                self.download_name = name;
                self.page = Page::Downloads;
                match store {
                    GameStore::Xbox => {
                        if let Err(error) = self.downloads.start_xbox(
                            &id,
                            &self.download_name,
                            &self.settings.library_dir,
                        ) {
                            self.downloads.notice = error;
                        }
                    }
                    GameStore::Epic => {
                        if let Some(game) = self.epic_catalog.game(&id).cloned() {
                            if let Err(error) =
                                self.downloads.start_epic(&game, &self.settings.library_dir)
                            {
                                self.downloads.notice = error;
                            }
                        } else {
                            self.downloads.notice = "Epic game metadata is unavailable. Refresh the Epic catalog and try again.".into();
                        }
                    }
                }
            }
            Action::CloseGame => {
                self.trailer.close();
                self.selected = None;
            }
            Action::PlayTrailer { title, url } => self.trailer.open(title, url),
            Action::CloseTrailer => self.trailer.close(),
            Action::RefreshCatalog => {
                self.images.clear_failed();
                self.selected = None;
                self.catalog.refresh();
            }
            Action::RefreshEpicCatalog => {
                self.images.clear_failed();
                self.selected = None;
                self.epic_catalog.refresh();
            }
            Action::ResetCatalogFilters => self.catalog.reset_filters(),
            Action::ResetEpicCatalogFilters => self.epic_catalog.reset_filters(),
            Action::ScanLibrary => self.scan_library(),
            Action::PlayGame { name, path } => {
                let epic_exe = self
                    .epic_catalog
                    .games
                    .iter()
                    .find(|g| g.name == name)
                    .map(|g| g.launch_exe.as_str())
                    .filter(|exe| !exe.is_empty());
                self.library.status = match library::launch_game(&path, epic_exe) {
                    Ok(()) => format!("Launching {name}…"),
                    Err(error) => error,
                };
            }
            Action::AskUninstall(entry) => self.pending_uninstall = Some(entry),
            Action::DismissUninstall => self.pending_uninstall = None,
            Action::ConfirmUninstall => {
                if let Some(entry) = self.pending_uninstall.take() {
                    self.library.uninstall(&entry, &self.settings.library_dir);
                    self.rematch_library();
                }
            }
        }
    }

    // ───────────────────────── Interface ─────────────────────────

    fn show_page(&mut self, ui: &mut egui::Ui, out: &mut Vec<Action>) {
        match self.page {
            Page::Home => home::show(
                ui,
                &self.catalog,
                &self.epic_catalog,
                &self.library,
                &mut self.images,
                out,
            ),
            Page::Catalog => match self.selected_catalog {
                GameStore::Xbox => catalog_page::show(
                    ui,
                    &mut self.catalog,
                    self.selected_catalog,
                    &self.library,
                    &mut self.images,
                    out,
                ),
                GameStore::Epic => catalog_page::show(
                    ui,
                    &mut self.epic_catalog,
                    self.selected_catalog,
                    &self.library,
                    &mut self.images,
                    out,
                ),
            },
            Page::Library => library_page::show(
                ui,
                &mut self.library,
                &self.catalog,
                &mut self.settings,
                &mut self.images,
                out,
            ),
            Page::Downloads => {
                downloads_page::show(ui, &self.catalog, &mut self.images, &mut self.downloads)
            }
            Page::Updates => updates_page::show(ui, &self.library, &mut self.updates),
            Page::Settings => settings_page::show(ui, &mut self.settings),
        }
    }

    fn show_chrome(&mut self, root: &mut egui::Ui, out: &mut Vec<Action>) {
        root.winui(|ui| {
            let area = ui.ctx().input(|i| i.viewport_rect());
            let side = egui::Rect::from_min_size(area.min, egui::vec2(SIDEBAR_W, area.height()));
            let main = egui::Rect::from_min_max(
                egui::pos2(side.max.x + MAIN_MARGIN, area.min.y),
                egui::pos2(
                    (area.max.x - MAIN_MARGIN).max(side.max.x + 300.0),
                    area.max.y,
                ),
            );

            ui.painter().rect_filled(area, 0, theme::BG_APP);
            ui.painter().rect_filled(side, 0, theme::BG_SIDEBAR);
            ui.painter().vline(
                side.max.x,
                area.y_range(),
                ui.visuals().widgets.noninteractive.bg_stroke,
            );

            let page = self.page;
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(side.shrink2(egui::vec2(18.0, 0.0)))
                    .layout(egui::Layout::top_down(egui::Align::Min)),
                |ui| sidebar::show(ui, page, out),
            );
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(main)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
                |ui| self.show_page(ui, out),
            );
        });
    }

    fn show_modals(&mut self, ctx: &egui::Context, frame: &eframe::Frame, out: &mut Vec<Action>) {
        let interactive = !self.trailer.is_open();
        let selected_game = self.selected.as_ref().and_then(|(store, id)| match store {
            GameStore::Xbox => self.catalog.game(id),
            GameStore::Epic => self.epic_catalog.game(id),
        });
        if let Some(game) = selected_game {
            game_modal::show(
                ctx,
                game,
                &mut self.modal,
                &mut self.images,
                interactive,
                out,
            );
        }
        if let Some(entry) = &self.pending_uninstall {
            uninstall_modal::show(ctx, entry, out);
        }
        trailer_overlay::show(ctx, frame, &mut self.trailer, out);
    }
}

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        self.poll(&ctx);

        if !self.backdrop_applied {
            if let Some(window) = frame.winit_window() {
                winui::sync_window_backdrop(window, FluentTheme::Dark, BackdropKind::Mica);
                self.backdrop_applied = true;
            }
        }

        let mut actions = Vec::new();
        self.show_chrome(root, &mut actions);
        self.show_modals(&ctx, frame, &mut actions);

        if !actions.is_empty() {
            for action in actions {
                self.apply(action);
            }
            ctx.request_repaint();
        }
    }
}
