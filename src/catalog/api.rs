//! Accès réseau au catalogue (liste de jeux + détails Microsoft) et parsing JSON.
use super::CatalogEvent;
use crate::model::{Game, GameAttribute, Trailer};
use crate::net;
use crate::util::{GIB, format_date};
use reqwest::blocking::Client;
use serde_json::Value;
use std::sync::mpsc::Sender;
use std::time::Duration;

const CATALOG_KEY: &str = "9eEo0ksYvZTsXgPiJUz7hQCio18hW+mPlQhy0UBq+10=";
const CATALOG_URLS: [&str; 2] = [
    "https://xboxapi.netlify.app/games",
    "https://raw.githubusercontent.com/MidacoYT/Games/refs/heads/main/games.json",
];
const DETAILS_URL: &str = "https://displaycatalog.mp.microsoft.com/v7.0/products";
const BATCH_SIZE: usize = 10;

/// Charge la liste puis enrichit chaque jeu par lots, dans un thread dédié.
pub fn spawn_refresh(tx: Sender<CatalogEvent>) {
    std::thread::spawn(move || run(tx));
}

fn run(tx: Sender<CatalogEvent>) {
    let client = match net::client(20) {
        Ok(client) => client,
        Err(error) => {
            let _ = tx.send(CatalogEvent::Done(Err(error.to_string())));
            return;
        }
    };
    let mut games = match download_catalog(&client) {
        Ok(games) => games,
        Err(error) => {
            let _ = tx.send(CatalogEvent::Done(Err(error)));
            return;
        }
    };
    let _ = tx.send(CatalogEvent::Initial(games.clone()));
    let enriched = enrich(&client, &mut games, &tx);
    let _ = tx.send(CatalogEvent::Done(Ok(enriched)));
}

fn download_catalog(client: &Client) -> Result<Vec<Game>, String> {
    let mut errors = Vec::new();
    for (index, url) in CATALOG_URLS.iter().enumerate() {
        let mut request = client.get(*url);
        if index == 0 && !CATALOG_KEY.is_empty() {
            request = request.bearer_auth(CATALOG_KEY);
        }
        match request
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.json::<Value>())
        {
            Ok(data) => match parse_catalog(&data) {
                Ok(games) => return Ok(games),
                Err(error) => errors.push(error),
            },
            Err(error) => errors.push(format!("{url}: {error}")),
        }
    }
    Err(errors.join("\n"))
}

fn enrich(client: &Client, games: &mut [Game], tx: &Sender<CatalogEvent>) -> usize {
    let mut enriched = 0;
    for batch in games.chunks_mut(BATCH_SIZE) {
        let jobs: Vec<_> = batch
            .iter()
            .enumerate()
            .map(|(index, game)| {
                let client = client.clone();
                let id = game.id.clone();
                std::thread::spawn(move || (index, fetch_details(&client, &id)))
            })
            .collect();
        let mut updates = Vec::new();
        for job in jobs {
            if let Ok((index, Some(product))) = job.join() {
                apply_details(&mut batch[index], &product);
                enriched += 1;
                updates.push(batch[index].clone());
            }
        }
        if !updates.is_empty() {
            let _ = tx.send(CatalogEvent::Updated(updates));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    enriched
}

// ───────────────────────── Parsing ─────────────────────────

fn catalog_items(data: &Value) -> Vec<&Value> {
    if let Some(array) = data.as_array() {
        array.iter().collect()
    } else if let Some(array) = data
        .get("Products")
        .or_else(|| data.get("items"))
        .and_then(Value::as_array)
    {
        array.iter().collect()
    } else if let Some(object) = data.as_object() {
        object.values().collect()
    } else {
        Vec::new()
    }
}

pub fn parse_catalog(data: &Value) -> Result<Vec<Game>, String> {
    let mut seen = std::collections::HashSet::new();
    let mut games: Vec<Game> = catalog_items(data)
        .into_iter()
        .filter_map(|item| {
            let id = item
                .get("productId")
                .or_else(|| item.get("ProductId"))?
                .as_str()?
                .trim();
            if id.is_empty() || !seen.insert(id.to_owned()) {
                return None;
            }
            Some(Game {
                id: id.into(),
                name: item
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|n| !n.trim().is_empty())
                    .unwrap_or(id)
                    .into(),
                ..Default::default()
            })
        })
        .collect();
    games.sort_by_key(|g| g.name.to_lowercase());
    for game in &mut games {
        game.refresh_search_key();
    }
    if games.is_empty() {
        return Err("The catalog contains no usable games.".into());
    }
    Ok(games)
}

fn fetch_details(client: &Client, id: &str) -> Option<Value> {
    let response = client
        .get(DETAILS_URL)
        .query(&[
            ("bigIds", id),
            ("market", "US"),
            ("languages", "en-us"),
            ("MS-CV", "DGU1mcuYo0WMMp+F.1"),
        ])
        .send()
        .ok()?
        .error_for_status()
        .ok()?
        .json::<Value>()
        .ok()?;
    let product = response.get("Products")?.as_array()?.first()?;
    product.is_object().then(|| product.clone())
}

fn absolute_url(uri: &str) -> String {
    if uri.starts_with("http") {
        uri.to_owned()
    } else {
        format!("https:{uri}")
    }
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or("").to_owned()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn image_for(images: &[Value], purpose: &str) -> String {
    images
        .iter()
        .find(|img| img["ImagePurpose"] == purpose)
        .and_then(|img| img["Uri"].as_str())
        .map(absolute_url)
        .unwrap_or_default()
}

/// Copie les informations détaillées d'un produit Microsoft dans `game`.
pub fn apply_details(game: &mut Game, product: &Value) {
    let localized = &product["LocalizedProperties"][0];
    let images = localized["Images"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    if let Some(title) = localized["ProductTitle"].as_str() {
        game.name = title.to_owned();
    }
    game.image = image_for(images, "BoxArt");
    let hero = image_for(images, "SuperHeroArt");
    game.hero = if hero.is_empty() {
        image_for(images, "TitledHeroArt")
    } else {
        hero
    };
    game.developer = text(&localized["DeveloperName"]);
    game.publisher = text(&localized["PublisherName"]);
    game.publisher_website = text(&localized["PublisherWebsiteUri"]);
    game.support_uri = text(&localized["SupportUri"]);
    game.language = text(&localized["Language"]);
    game.last_modified = format_date(product["LastModifiedDate"].as_str().unwrap_or(""));
    game.date_added = product["DateAdded"].as_str().unwrap_or_default().to_owned();
    game.release = format_date(
        localized["ReleaseDate"]
            .as_str()
            .or(product["ReleaseDate"].as_str())
            .or(product["DisplaySkuAvailabilities"][0]["Availabilities"][0]["Properties"]
                ["OriginalReleaseDate"]
                .as_str())
            .unwrap_or(""),
    );
    game.description = localized["Description"]
        .as_str()
        .or(localized["ShortDescription"].as_str())
        .or(localized["ProductDescription"].as_str())
        .unwrap_or("")
        .to_owned();

    let size = product["DisplaySkuAvailabilities"][0]["Sku"]["Properties"]["Packages"][0]
        ["MaxDownloadSizeInBytes"]
        .as_u64()
        .or(product["InstallAttributes"][0]["Size"].as_u64());
    if let Some(bytes) = size {
        game.size = format!("{:.1} GB", bytes as f64 / GIB);
    }

    game.platform = strings(&product["Platforms"]).join(" · ");
    game.genres = strings(&localized["Genres"]);
    let categories = if product["Properties"]["Categories"].is_array() {
        &product["Properties"]["Categories"]
    } else {
        &localized["Categories"]
    };
    game.categories = strings(categories);

    // Une capture sur deux : l'API renvoie plusieurs résolutions de la même image.
    game.screenshots = images
        .iter()
        .filter(|img| img["ImagePurpose"] == "Screenshot")
        .step_by(2)
        .filter_map(|img| img["Uri"].as_str())
        .map(absolute_url)
        .collect();
    game.trailers = localized["CMSVideos"]
        .as_array()
        .map(|videos| videos.iter().map(parse_trailer).collect())
        .unwrap_or_default();
    game.attributes = product["Properties"]["Attributes"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item["Name"].as_str().map(|name| GameAttribute {
                        name: name.to_owned(),
                        min: item["Minimum"].as_u64(),
                        max: item["Maximum"].as_u64(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    game.achievements = product["Achievements"]["AchievementCount"].as_u64();
    game.gamerscore = product["Achievements"]["GamerscoreTotal"].as_u64();
    game.rating_count = parse_rating_count(product);
    game.rating = parse_rating(localized, product);
    game.players = parse_players(product);
    game.ea_play = product["IsEAPlay"].as_bool().unwrap_or(false);
    game.refresh_search_key();
}

fn parse_trailer(video: &Value) -> Trailer {
    Trailer {
        caption: text(&video["Caption"]),
        preview: video["PreviewImage"]["Uri"]
            .as_str()
            .map(absolute_url)
            .unwrap_or_default(),
        url: video["DASH"]
            .as_str()
            .or(video["HLS"].as_str())
            .unwrap_or("")
            .to_owned(),
    }
}

fn parse_rating_count(product: &Value) -> u64 {
    product["MarketProperties"][0]["UsageData"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["AggregateTimeSpan"] == "AllTime")
                .or_else(|| items.first())
        })
        .and_then(|item| {
            item["RatingCount"]
                .as_u64()
                .or_else(|| item["RatingCount"].as_str().and_then(|v| v.parse().ok()))
        })
        .unwrap_or(0)
}

fn parse_rating(localized: &Value, product: &Value) -> String {
    let ratings = localized["ContentRatings"].as_array();
    let find = |system: &str| {
        ratings.and_then(|items| items.iter().find(|r| r["RatingSystem"] == system))
    };
    find("PEGI")
        .or_else(|| find("ESRB"))
        .and_then(|rating| {
            let system = rating["RatingSystem"].as_str()?;
            let value = rating["RatingId"].as_str()?;
            Some(if system == "PEGI" {
                if value.to_ascii_lowercase().contains("pegi") {
                    value.to_owned()
                } else {
                    format!("PEGI {value}")
                }
            } else {
                format!("ESRB {value}")
            })
        })
        .or_else(|| {
            let age = product["MarketProperties"][0]["MinimumUserAge"].as_u64()?;
            (age > 0).then(|| format!("PEGI {age}"))
        })
        .unwrap_or_default()
}

fn parse_players(product: &Value) -> String {
    let attributes = product["Properties"]["Attributes"].as_array();
    attributes
        .and_then(|items| items.iter().find(|item| item["Name"] == "XblOnlineCoop"))
        .map(|coop| {
            let min = coop["Minimum"].as_u64().unwrap_or(1);
            let max = coop["Maximum"].as_u64().unwrap_or(4);
            if min == max {
                format!("{min} player{}", if min > 1 { "s" } else { "" })
            } else {
                format!("{min}–{max} players")
            }
        })
        .or_else(|| attributes.map(|_| "1 player".to_owned()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_deduplicates_and_sorts() {
        let data = serde_json::json!([
            {"productId": "B", "name": "Zulu"},
            {"productId": "A", "name": "Alpha"},
            {"productId": "B"}
        ]);
        let games = parse_catalog(&data).unwrap();
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].id, "A");
    }

    #[test]
    fn catalog_accepts_products_shape() {
        let data = serde_json::json!({"Products": [{"ProductId": "ABC"}]});
        assert_eq!(parse_catalog(&data).unwrap()[0].name, "ABC");
    }

    #[test]
    fn empty_catalog_is_an_error() {
        assert!(parse_catalog(&serde_json::json!([])).is_err());
    }
}
