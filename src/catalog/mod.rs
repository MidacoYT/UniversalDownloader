//! État du catalogue : chargement asynchrone, filtres, tri, pagination.
pub mod api;

use crate::library::Library;
pub use crate::model::{ALL_GENRES, ALL_PLATFORMS};
use crate::model::{Game, GameStore};
use crate::util::{size_in_gb, top_k};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver};

pub const PAGE_SIZE: usize = 24;
pub const EPIC_API_BASE: &str = "https://epicapi.netlify.app";
pub static PLATFORMS: [&str; 4] = [ALL_PLATFORMS, "PC", "Xbox", "Cloud"];

#[derive(PartialEq, Clone, Copy)]
pub enum CatalogSort {
    Name,
    Size,
    MostReviewed,
}

impl CatalogSort {
    pub const ALL: [Self; 3] = [Self::Name, Self::Size, Self::MostReviewed];
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Alphabetical",
            Self::Size => "Largest",
            Self::MostReviewed => "Most reviewed",
        }
    }
}

#[derive(PartialEq, Clone, Copy)]
pub enum InstalledFilter {
    All,
    Installed,
    Available,
}

impl InstalledFilter {
    pub const ALL: [Self; 3] = [Self::All, Self::Installed, Self::Available];
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All games",
            Self::Installed => "Installed",
            Self::Available => "Available",
        }
    }
}

pub enum CatalogEvent {
    Initial(Vec<Game>),
    Updated(Vec<Game>),
    Done(Result<usize, String>),
}

pub struct Catalog {
    pub store: GameStore,
    pub games: Vec<Game>,
    pub search: String,
    pub platform_filter: &'static str,
    pub genre_filter: String,
    pub installed_filter: InstalledFilter,
    pub sort: CatalogSort,
    pub status: String,
    /// Indices dans `games` après filtrage + tri.
    pub visible: Vec<usize>,
    /// Genres disponibles (pour les puces de filtre).
    pub genres: Vec<String>,
    pub page_index: usize,
    pub scroll_top: bool,
    rx: Option<Receiver<CatalogEvent>>,
    filter_key: String,
    dirty: bool,
    /// Incrémenté à chaque modification des données : permet aux catalogues dérivés
    /// (vue « All Catalog ») de ne se reconstruire que quand c'est nécessaire.
    revision: u64,
    /// Révisions (Xbox, Epic) utilisées lors de la dernière fusion dans `sync_combined`.
    merged_from: Option<(u64, u64)>,
}

impl Catalog {
    pub fn new(store: GameStore) -> Self {
        Self {
            store,
            games: Vec::new(),
            search: String::new(),
            platform_filter: ALL_PLATFORMS,
            genre_filter: ALL_GENRES.into(),
            installed_filter: InstalledFilter::All,
            sort: CatalogSort::Name,
            status: String::new(),
            visible: Vec::new(),
            genres: Vec::new(),
            page_index: 0,
            scroll_top: false,
            rx: None,
            filter_key: String::new(),
            dirty: true,
            revision: 0,
            merged_from: None,
        }
    }

    pub fn is_loading(&self) -> bool {
        self.rx.is_some()
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
        self.revision = self.revision.wrapping_add(1);
    }

    /// Reconstruit ce catalogue comme la concaténation de `xbox` et `epic`, uniquement si
    /// l'un des deux a changé depuis la dernière fusion (avant : clone complet à chaque frame).
    pub fn sync_combined(&mut self, xbox: &Catalog, epic: &Catalog) {
        let sources = (xbox.revision, epic.revision);
        if self.merged_from == Some(sources) {
            return;
        }
        self.merged_from = Some(sources);
        self.games.clear();
        self.games.reserve(xbox.games.len() + epic.games.len());
        self.games.extend(xbox.games.iter().cloned());
        self.games.extend(epic.games.iter().cloned());
        self.status = format!(
            "{} Xbox · {} Epic games{}",
            xbox.games.len(),
            epic.games.len(),
            if xbox.is_loading() || epic.is_loading() {
                " · loading"
            } else {
                ""
            }
        );
        self.mark_dirty();
    }

    pub fn game(&self, id: &str) -> Option<&Game> {
        self.games.iter().find(|g| g.id == id)
    }

    pub fn top_games(&self, count: usize) -> Vec<&Game> {
        top_k(self.games.iter(), count, |a, b| {
            b.rating_count.cmp(&a.rating_count)
        })
    }

    pub fn reset_filters(&mut self) {
        self.search.clear();
        self.platform_filter = ALL_PLATFORMS;
        self.genre_filter = ALL_GENRES.into();
        self.installed_filter = InstalledFilter::All;
        self.sort = CatalogSort::Name;
    }

    pub fn total_pages(&self) -> usize {
        self.visible.len().div_ceil(PAGE_SIZE).max(1)
    }

    pub fn page_games(&self) -> Vec<&Game> {
        let start = self.page_index * PAGE_SIZE;
        let end = (start + PAGE_SIZE).min(self.visible.len());
        self.visible
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .map(|&i| &self.games[i])
            .collect()
    }

    pub fn refresh(&mut self) {
        if self.is_loading() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.games.clear();
        self.visible.clear();
        self.mark_dirty();
        self.status = match self.store {
            GameStore::Xbox => "Loading the Xbox catalog…".into(),
            GameStore::Epic => "Loading the Epic Games catalog…".into(),
        };
        api::spawn_refresh(tx, self.store);
    }

    /// Consomme les événements réseau. Retourne `true` si la bibliothèque doit être ré-associée.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        let events: Vec<_> = rx.try_iter().collect();
        let (mut rematch, mut finished) = (false, false);
        for event in events {
            match event {
                CatalogEvent::Initial(games) => {
                    self.games = games;
                    self.mark_dirty();
                    rematch = true;
                    self.status = match self.store {
                        GameStore::Xbox => format!(
                            "{} games found — loading Microsoft details…",
                            self.games.len()
                        ),
                        GameStore::Epic => format!("{} Epic games found.", self.games.len()),
                    };
                }
                CatalogEvent::Updated(updates) => {
                    // Index id → position construit une seule fois (avant : recherche linéaire
                    // par jeu mis à jour, soit O(n²) sur un catalogue de plusieurs milliers).
                    let positions: HashMap<&str, usize> = self
                        .games
                        .iter()
                        .enumerate()
                        .map(|(index, game)| (game.id.as_str(), index))
                        .collect();
                    let targets: Vec<(usize, Game)> = updates
                        .into_iter()
                        .filter_map(|updated| {
                            positions.get(updated.id.as_str()).map(|&i| (i, updated))
                        })
                        .collect();
                    for (index, updated) in targets {
                        self.games[index] = updated;
                    }
                    self.mark_dirty();
                }
                CatalogEvent::Done(Ok(count)) => {
                    self.status = match self.store {
                        GameStore::Xbox => format!(
                            "{} games — {} detailed profiles loaded",
                            self.games.len(),
                            count
                        ),
                        GameStore::Epic => {
                            format!("{} Epic manifests available.", self.games.len())
                        }
                    };
                    finished = true;
                    rematch = true;
                }
                CatalogEvent::Done(Err(error)) => {
                    self.status = format!("Catalog error: {error}");
                    finished = true;
                }
            }
        }
        if finished {
            self.rx = None;
            self.mark_dirty();
        }
        rematch
    }

    /// Recalcule la liste filtrée uniquement si un filtre ou les données ont changé.
    pub fn refresh_visible(&mut self, library: &Library) {
        let key = format!(
            "{}\u{1}{}\u{1}{}\u{1}{}\u{1}{}",
            self.search.trim().to_lowercase(),
            self.platform_filter,
            self.genre_filter,
            self.installed_filter.label(),
            self.sort.label()
        );
        if !self.dirty && key == self.filter_key {
            return;
        }
        if key != self.filter_key {
            self.page_index = 0;
        }
        self.filter_key = key;
        self.dirty = false;
        self.genres = collect_genres(&self.games);

        let query = self.search.trim().to_lowercase();
        let genre = self.genre_filter.to_lowercase();
        let loading = self.is_loading();
        let installed = library.installed_ids();
        self.visible = self
            .games
            .iter()
            .enumerate()
            .filter(|(_, g)| {
                (query.is_empty() || g.search_key.contains(&query))
                    && g.matches_platform(self.platform_filter, loading)
                    && g.matches_genre(&genre)
                    && match self.installed_filter {
                        InstalledFilter::All => true,
                        InstalledFilter::Installed => installed.contains(g.id.as_str()),
                        InstalledFilter::Available => !installed.contains(g.id.as_str()),
                    }
            })
            .map(|(i, _)| i)
            .collect();
        self.sort_visible();
    }

    fn sort_visible(&mut self) {
        let games = &self.games;
        match self.sort {
            CatalogSort::Name => self
                .visible
                .sort_by_cached_key(|&i| games[i].name.to_lowercase()),
            // Taille parsée une seule fois par jeu (et non à chaque comparaison).
            CatalogSort::Size => self.visible.sort_by_cached_key(|&i| {
                std::cmp::Reverse((size_in_gb(&games[i].size) * 1024.0) as u64)
            }),
            CatalogSort::MostReviewed => self
                .visible
                .sort_by(|&a, &b| games[b].rating_count.cmp(&games[a].rating_count)),
        }
    }
}

fn collect_genres(games: &[Game]) -> Vec<String> {
    let mut genres: Vec<String> = games
        .iter()
        .flat_map(|g| g.genres.iter().chain(g.categories.iter()).cloned())
        .filter(|g| !g.trim().is_empty())
        .collect();
    genres.sort_by_key(|g| g.to_lowercase());
    genres.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    genres
}
