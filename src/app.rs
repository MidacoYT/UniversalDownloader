//! Composition de l'application : état global, routage des pages, application des actions.
use crate::catalog::Catalog;
use crate::catalog::epic_store::EpicStoreInfo;
use crate::downloads::DownloadManager;
use crate::images::ImageCache;
use crate::library::{self, Library, LibraryEntry};
use crate::model::GameStore;
use crate::player::TrailerPlayer;
use crate::settings::Settings;
use crate::ui::game_modal::ModalState;
use crate::ui::{
    Action, CatalogSelection, Page, catalog_page, download_location_modal, downloads_page,
    game_modal, home, library_page, settings_page, sidebar, theme, topbar, trailer_overlay,
    uninstall_modal, updates_page, window,
};
use crate::updates::UpdateCheck;
use eframe::egui;
use fluent_egui::winui::prelude::*;
use fluent_egui::winui::{self, BackdropKind, FluentTheme};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

const MAIN_MARGIN: f32 = 28.0;

struct PendingDownload {
    id: String,
    name: String,
    store: GameStore,
    directory: String,
    generation: u64,
}

pub struct App {
    page: Page,
    catalog: Catalog,
    epic_catalog: Catalog,
    epic_stats_tx: Sender<(String, Result<(Option<u8>, Option<u8>), String>)>,
    epic_stats_rx: Receiver<(String, Result<(Option<u8>, Option<u8>), String>)>,
    epic_info_tx: Sender<(String, Result<Option<EpicStoreInfo>, String>)>,
    epic_info_rx: Receiver<(String, Result<Option<EpicStoreInfo>, String>)>,
    /// Requêtes boutique Epic en cours (garde l'interface réveillée jusqu'à la réponse).
    epic_info_pending: usize,
    shortcut_tx: Sender<String>,
    shortcut_rx: Receiver<String>,
    shortcut_checked: HashSet<u64>,
    selected_catalog: CatalogSelection,
    combined_catalog: Catalog,
    library: Library,
    settings: Settings,
    updates: UpdateCheck,
    downloads: DownloadManager,
    pending_download: Option<PendingDownload>,
    download_prompt_generation: u64,
    images: ImageCache,
    selected: Option<(GameStore, String)>,
    modal: ModalState,
    pending_uninstall: Option<LibraryEntry>,
    trailer: TrailerPlayer,
    backdrop_applied: bool,
    /// Date du dernier scan bibliothèque : déclenche le debounce du changement d'onglet.
    last_library_scan: Instant,
}

impl App {
    pub fn new() -> Self {
        let (epic_stats_tx, epic_stats_rx) = mpsc::channel();
        let (shortcut_tx, shortcut_rx) = mpsc::channel();
        let (epic_info_tx, epic_info_rx) = mpsc::channel();
        let mut app = Self {
            page: Page::Home,
            catalog: Catalog::new(GameStore::Xbox),
            epic_catalog: Catalog::new(GameStore::Epic),
            epic_stats_tx,
            epic_stats_rx,
            epic_info_tx,
            epic_info_rx,
            epic_info_pending: 0,
            shortcut_tx,
            shortcut_rx,
            shortcut_checked: HashSet::new(),
            selected_catalog: CatalogSelection::All,
            combined_catalog: Catalog::new(GameStore::Xbox),
            library: Library::new(),
            settings: Settings::load(),
            updates: UpdateCheck::new(),
            downloads: DownloadManager::new(),
            pending_download: None,
            download_prompt_generation: 0,
            images: ImageCache::new(),
            selected: None,
            modal: ModalState::default(),
            pending_uninstall: None,
            trailer: TrailerPlayer::new(),
            backdrop_applied: false,
            last_library_scan: Instant::now(),
        };
        app.catalog.refresh();
        app.epic_catalog.refresh();
        app.scan_library();
        app
    }

    // ───────────────────────── Logique ─────────────────────────

    fn scan_library(&mut self) {
        self.settings.persist();
        self.last_library_scan = Instant::now();
        self.library.scan(
            &self.settings.library_dir,
            self.downloads.incomplete_library_paths(),
        );
    }

    fn rematch_library(&mut self) {
        self.library
            .rematch(self.catalog.games.iter().chain(&self.epic_catalog.games));
        self.catalog.mark_dirty();
        self.epic_catalog.mark_dirty();
    }

    /// Complète une fois la fiche d'un jeu Epic : infos officielles de la boutique Epic Games
    /// (éditeur, développeur, date, page) et note OpenCritic.
    fn request_epic_details(&mut self, id: &str) {
        let Some(game) = self
            .epic_catalog
            .games
            .iter_mut()
            .find(|game| game.id == id)
        else {
            return;
        };
        if !game.store_info_requested {
            game.store_info_requested = true;
            let (game_id, name) = (game.id.clone(), game.name.clone());
            let tx = self.epic_info_tx.clone();
            self.epic_info_pending += 1;
            std::thread::spawn(move || {
                let _ = tx.send((game_id, crate::catalog::epic_store::fetch(&name)));
            });
        }
        if !game.egdata_offer_id.is_empty() && !game.epic_stats_requested {
            game.epic_stats_requested = true;
            let (game_id, offer_id) = (game.id.clone(), game.egdata_offer_id.clone());
            let tx = self.epic_stats_tx.clone();
            std::thread::spawn(move || {
                let result = crate::catalog::api::fetch_epic_critic_stats(&offer_id);
                let _ = tx.send((game_id, result));
            });
        }
    }

    /// Crée le raccourci bureau sans bloquer l'interface (PowerShell prend ~1 s).
    fn create_shortcut_in_background(
        &self,
        name: String,
        folder: PathBuf,
        store: GameStore,
        game_id: Option<String>,
    ) {
        let catalog = match store {
            GameStore::Xbox => &self.catalog,
            GameStore::Epic => &self.epic_catalog,
        };
        let product_id = game_id.clone();
        let launch_exe = game_id
            .and_then(|id| catalog.games.iter().find(|game| game.id == id))
            .map(|game| game.launch_exe.clone())
            .filter(|exe| !exe.is_empty());
        let tx = self.shortcut_tx.clone();
        std::thread::spawn(move || {
            let message = match library::create_shortcut(
                &name,
                &folder,
                store,
                launch_exe.as_deref(),
                product_id.as_deref(),
            ) {
                Ok(path) => format!("Desktop shortcut created: {}", path.display()),
                Err(error) => format!("Could not create desktop shortcut: {error}"),
            };
            let _ = tx.send(message);
        });
    }

    /// Intègre les résultats des threads d'arrière-plan.
    fn poll(&mut self, ctx: &egui::Context) {
        self.images.poll(ctx);
        let catalog_changed = self.catalog.poll();
        let epic_catalog_changed = self.epic_catalog.poll();
        if !self.epic_catalog.is_loading() {
            self.downloads
                .resume_restored_epic(&self.epic_catalog.games);
        }
        let epic_stats_updates: Vec<_> = self.epic_stats_rx.try_iter().collect();
        let epic_stats_changed = !epic_stats_updates.is_empty();
        if epic_stats_changed {
            self.epic_catalog.mark_dirty();
        }
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
        let epic_info_updates: Vec<_> = self.epic_info_rx.try_iter().collect();
        let epic_info_changed = !epic_info_updates.is_empty();
        self.epic_info_pending = self
            .epic_info_pending
            .saturating_sub(epic_info_updates.len());
        for (id, result) in epic_info_updates {
            let Some(game) = self
                .epic_catalog
                .games
                .iter_mut()
                .find(|game| game.id == id)
            else {
                continue;
            };
            match result {
                Ok(Some(info)) => {
                    info.apply(game);
                    game.store_info_status = "Source: Epic Games store".into();
                }
                Ok(None) => {
                    game.store_info_status =
                        "No matching Epic Games store listing — showing catalog data.".into();
                }
                Err(error) => {
                    game.store_info_status = format!("Epic Games store info unavailable: {error}");
                }
            }
        }
        if epic_info_changed {
            self.epic_catalog.mark_dirty();
        }
        let library_changed = self.library.poll();
        if self.library.poll_uninstall() {
            self.rematch_library();
        }
        self.updates.poll();
        if self.updates.take_restart_requested() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let completed_before: HashSet<u64> = self
            .downloads
            .entries
            .iter()
            .filter(|entry| entry.status == crate::downloads::DownloadStatus::Completed)
            .map(|entry| entry.id)
            .collect();
        self.downloads.poll();
        let download_changed_library = self.downloads.take_library_change();
        // Comme Xbox ou Epic : le raccourci bureau est créé automatiquement
        // dès qu'une installation vient de se terminer.
        let new_installs: Vec<_> = self
            .downloads
            .entries
            .iter()
            .filter(|entry| {
                entry.status == crate::downloads::DownloadStatus::Completed
                    && !completed_before.contains(&entry.id)
            })
            .map(|entry| {
                (
                    entry.name.clone(),
                    entry.install_folder().to_path_buf(),
                    entry.store(),
                    entry.game_id().map(str::to_owned),
                )
            })
            .collect();
        let shortcuts_pending: Vec<_> = if self.settings.auto_shortcut {
            self.downloads
                .entries
                .iter()
                .filter(|entry| {
                    entry.status == crate::downloads::DownloadStatus::Completed
                        && !self.shortcut_checked.contains(&entry.id)
                })
                .map(|entry| {
                    (
                        entry.id,
                        entry.name.clone(),
                        entry.install_folder().to_path_buf(),
                        entry.store(),
                        entry.game_id().map(str::to_owned),
                    )
                })
                .collect()
        } else {
            Vec::new()
        };
        if !new_installs.is_empty() || download_changed_library {
            self.scan_library();
        }
        for (id, name, folder, store, game_id) in shortcuts_pending {
            self.shortcut_checked.insert(id);
            self.create_shortcut_in_background(name, folder, store, game_id);
        }
        if let Some(message) = self.shortcut_rx.try_iter().last() {
            self.library.status = message;
        }
        if catalog_changed || epic_catalog_changed || library_changed {
            self.rematch_library();
        }
        if self.catalog.is_loading()
            || self.epic_catalog.is_loading()
            || epic_catalog_changed
            || epic_stats_changed
            || epic_info_changed
            || self.epic_info_pending > 0
            || self.library.is_scanning()
            || self.library.is_uninstalling()
            || self.updates.is_loading()
            || self.downloads.is_active()
        {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else if self
            .downloads
            .entries
            .iter()
            .any(|entry| entry.status == crate::downloads::DownloadStatus::Completed)
        {
            ctx.request_repaint_after(Duration::from_secs(2));
        }
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Navigate(page) => {
                if self.page != page {
                    self.page = page;
                    // Un onglet peut cacher des changements faits hors de l'app
                    // (installation manuelle, suppression de dossier) : on rescanne
                    // la bibliothèque et on retente la reprise des téléchargements Epic.
                    // Débounce : pas de scan si le dernier date de moins de 3 s.
                    if self.last_library_scan.elapsed() > Duration::from_secs(3) {
                        self.scan_library();
                    }
                    self.downloads
                        .resume_restored_epic(&self.epic_catalog.games);
                }
                if page == Page::Catalog {
                    match self.selected_catalog {
                        CatalogSelection::Xbox => {
                            if self.catalog.games.is_empty() && !self.catalog.is_loading() {
                                self.catalog.refresh();
                            }
                        }
                        CatalogSelection::Epic => {
                            if self.epic_catalog.games.is_empty() && !self.epic_catalog.is_loading()
                            {
                                self.epic_catalog.refresh();
                            }
                        }
                        CatalogSelection::All => {
                            if self.catalog.games.is_empty() && !self.catalog.is_loading() {
                                self.catalog.refresh();
                            }
                            if self.epic_catalog.games.is_empty() && !self.epic_catalog.is_loading()
                            {
                                self.epic_catalog.refresh();
                            }
                            self.combined_catalog.scroll_top = true;
                        }
                    }
                }
            }
            Action::SelectCatalog(selection) => {
                self.selected_catalog = selection;
                match selection {
                    CatalogSelection::Xbox => {
                        self.catalog.scroll_top = true;
                        if self.catalog.games.is_empty() && !self.catalog.is_loading() {
                            self.catalog.refresh();
                        }
                    }
                    CatalogSelection::Epic => {
                        self.epic_catalog.scroll_top = true;
                        if self.epic_catalog.games.is_empty() && !self.epic_catalog.is_loading() {
                            self.epic_catalog.refresh();
                        }
                    }
                    CatalogSelection::All => {
                        self.combined_catalog.scroll_top = true;
                        if self.catalog.games.is_empty() && !self.catalog.is_loading() {
                            self.catalog.refresh();
                        }
                        if self.epic_catalog.games.is_empty() && !self.epic_catalog.is_loading() {
                            self.epic_catalog.refresh();
                        }
                    }
                }
            }
            Action::OpenGame { id, store } => {
                self.modal = ModalState::default();
                if store == GameStore::Epic {
                    self.request_epic_details(&id);
                }
                self.selected = Some((store, id));
            }
            Action::PrepareDownload { id, name, store } => {
                self.trailer.close();
                self.selected = None;
                self.download_prompt_generation = self.download_prompt_generation.wrapping_add(1);
                self.pending_download = Some(PendingDownload {
                    id,
                    name,
                    store,
                    directory: self.settings.library_dir.clone(),
                    generation: self.download_prompt_generation,
                });
            }
            Action::ConfirmDownload { directory } => {
                if let Some(pending) = self.pending_download.take() {
                    self.page = Page::Downloads;
                    let result = match pending.store {
                        GameStore::Xbox => {
                            self.downloads
                                .start_xbox(&pending.id, &pending.name, &directory)
                        }
                        GameStore::Epic => {
                            if let Some(game) = self.epic_catalog.game(&pending.id).cloned() {
                                self.downloads.start_epic(&game, &directory)
                            } else {
                                Err("Epic game metadata is unavailable. Refresh the Epic catalog and try again.".into())
                            }
                        }
                    };
                    match result {
                        Ok(()) => {
                            self.settings.library_dir = directory;
                            self.settings.persist();
                            self.scan_library();
                        }
                        Err(error) => self.downloads.notice = error,
                    }
                }
            }
            Action::DismissDownload => {
                self.pending_download = None;
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
            Action::ResetAllCatalogFilters => self.combined_catalog.reset_filters(),
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
                    self.library
                        .start_uninstall(entry, &self.settings.library_dir);
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
                CatalogSelection::All => catalog_page::show_all(
                    ui,
                    &mut self.combined_catalog,
                    &self.catalog,
                    &self.epic_catalog,
                    &self.library,
                    &mut self.images,
                    out,
                ),
                CatalogSelection::Xbox => catalog_page::show(
                    ui,
                    &mut self.catalog,
                    self.selected_catalog,
                    &self.library,
                    &mut self.images,
                    out,
                ),
                CatalogSelection::Epic => catalog_page::show(
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
                &self.epic_catalog,
                &mut self.settings,
                &mut self.images,
                out,
            ),
            Page::Downloads => downloads_page::show(
                ui,
                &self.catalog,
                &self.epic_catalog,
                &mut self.images,
                &mut self.downloads,
            ),
            Page::Updates => updates_page::show(ui, &self.library, &mut self.updates),
            Page::Settings => settings_page::show(ui, &mut self.settings),
        }
    }

    fn show_chrome(&mut self, root: &mut egui::Ui, out: &mut Vec<Action>) {
        root.winui(|ui| {
            let area = ui.ctx().input(|i| i.viewport_rect());
            let bar = egui::Rect::from_min_size(area.min, egui::vec2(area.width(), topbar::HEIGHT));
            let main = egui::Rect::from_min_max(
                egui::pos2(area.min.x + MAIN_MARGIN, bar.max.y),
                egui::pos2(area.max.x - MAIN_MARGIN, area.max.y),
            );

            ui.painter().rect_filled(area, 0, theme::BG_APP);

            let page = self.page;
            let download = sidebar::DownloadSummary::current(
                &self.downloads,
                &self.catalog,
                &self.epic_catalog,
            );
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(bar)
                    .layout(egui::Layout::left_to_right(egui::Align::Min)),
                |ui| topbar::show(ui, page, download.as_ref(), out),
            );
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(main)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
                |ui| self.show_page(ui, out),
            );

            // En dernier : les poignées de redimensionnement passent au-dessus du contenu.
            window::resize_edges(ui, area);
            window::paint_border(ui.ctx(), area);
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
        if let Some(pending) = &mut self.pending_download {
            download_location_modal::show(
                ctx,
                &pending.name,
                pending.store,
                &mut pending.directory,
                pending.generation,
                out,
            );
        }
        if let Some(progress) = self.library.uninstall_progress() {
            uninstall_modal::show_progress(ctx, progress);
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
