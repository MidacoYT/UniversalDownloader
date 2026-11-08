//! Informations officielles d'un jeu Epic, lues directement sur la boutique Epic Games
//! (GraphQL publique de store.epicgames.com) plutôt que sur un site tiers.
use crate::model::Game;
use crate::net;
use crate::util::{is_known_release, normalize_title};
use serde_json::{Value, json};

const GRAPHQL_GET_URL: &str = "https://graphql.epicgames.com/graphql";
const GRAPHQL_POST_URL: &str = "https://store.epicgames.com/graphql";
const STORE_PAGE: &str = "https://store.epicgames.com/p/";
const QUERY: &str = r#"query searchStoreQuery($keywords: String, $country: String!, $locale: String, $count: Int) {
  Catalog {
    searchStore(keywords: $keywords, country: $country, locale: $locale, count: $count) {
      elements {
        title
        description
        effectiveDate
        seller { name }
        productSlug
        urlSlug
        customAttributes { key value }
        catalogNs { mappings(pageType: "productHome") { pageSlug pageType } }
        offerMappings { pageSlug pageType }
      }
    }
  }
}"#;

#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub struct EpicStoreInfo {
    pub developer: String,
    pub publisher: String,
    pub description: String,
    pub release_date: String,
    pub url: String,
}

/// Cherche le jeu par son titre sur la boutique Epic ; `Ok(None)` si aucune fiche ne correspond.
/// Les erreurs (réseau, protection anti-bot, schéma refusé) sont renvoyées pour être affichées.
pub fn fetch(name: &str) -> Result<Option<EpicStoreInfo>, String> {
    let client = net::shared_client().ok_or("HTTP client unavailable.")?;
    let variables = json!({ "keywords": name, "country": "US", "locale": "en-US", "count": 10 });
    let variables_text = variables.to_string();

    let get = client
        .get(GRAPHQL_GET_URL)
        .query(&[("query", QUERY), ("variables", variables_text.as_str())])
        .header("Accept", "application/json");
    let post = client
        .post(GRAPHQL_POST_URL)
        .header("Accept", "application/json")
        .json(&json!({ "query": QUERY, "variables": variables }));

    let mut errors = Vec::new();
    for request in [get, post] {
        match send(request) {
            Ok(data) => return Ok(parse(&data, name)),
            Err(error) => errors.push(error),
        }
    }
    Err(format!("Epic Games store unreachable ({})", errors.join(" / ")))
}

fn send(request: reqwest::blocking::RequestBuilder) -> Result<Value, String> {
    let data: Value = request
        .send()
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.json())
        .map_err(|error| error.to_string())?;
    if data.pointer("/data/Catalog").is_none_or(Value::is_null) {
        let message = data
            .pointer("/errors/0/message")
            .and_then(Value::as_str)
            .unwrap_or("empty response");
        return Err(format!("GraphQL: {message}"));
    }
    Ok(data)
}

impl EpicStoreInfo {
    /// Les données de la boutique Epic priment pour l'éditeur/développeur ; le reste ne
    /// remplit que les champs encore vides.
    pub fn apply(&self, game: &mut Game) {
        if !self.developer.is_empty() {
            game.developer = self.developer.clone();
        }
        if !self.publisher.is_empty() {
            game.publisher = self.publisher.clone();
        }
        if game.description.is_empty() && game.long_description.is_empty() {
            game.description = self.description.clone();
        }
        if game.release.is_empty() && !self.release_date.is_empty() {
            game.release_date_sort = self.release_date.clone();
            game.release = crate::util::format_date(&self.release_date);
        }
        if !self.url.is_empty() {
            game.store_url = self.url.clone();
        }
        game.refresh_search_key();
    }
}

fn parse(data: &Value, name: &str) -> Option<EpicStoreInfo> {
    let wanted = normalize_title(name);
    let elements = data
        .pointer("/data/Catalog/searchStore/elements")?
        .as_array()?;
    let title_of = |element: &Value| {
        element
            .get("title")
            .and_then(Value::as_str)
            .map(normalize_title)
            .unwrap_or_default()
    };
    // Titre identique en priorité ; sinon l'édition la plus courte qui commence par ce titre
    // (« Game – Standard Edition »).
    let element = elements
        .iter()
        .find(|element| title_of(element) == wanted)
        .or_else(|| {
            elements
                .iter()
                .filter(|element| !wanted.is_empty() && title_of(element).starts_with(&wanted))
                .min_by_key(|element| title_of(element).len())
        })?;

    let text = |value: Option<&Value>| -> String {
        value
            .and_then(Value::as_str)
            .map(|text| text.trim().to_owned())
            .unwrap_or_default()
    };
    let attribute = |key: &str| -> String {
        let value = element
            .get("customAttributes")
            .and_then(Value::as_array)
            .and_then(|list| {
                list.iter()
                    .find(|item| item.get("key").and_then(Value::as_str) == Some(key))
            })
            .and_then(|item| item.get("value"));
        text(value)
    };

    let mut publisher = attribute("publisherName");
    if publisher.is_empty() {
        publisher = text(element.pointer("/seller/name"));
    }
    let release_date = text(element.get("effectiveDate"));
    Some(EpicStoreInfo {
        developer: attribute("developerName"),
        publisher,
        description: text(element.get("description")),
        release_date: if is_known_release(&release_date) {
            release_date
        } else {
            String::new()
        },
        url: store_url(element).unwrap_or_default(),
    })
}

fn page_slug(mappings: Option<&Value>) -> Option<String> {
    let list = mappings?.as_array()?;
    let pick = |entry: &&Value| {
        entry
            .get("pageSlug")
            .and_then(Value::as_str)
            .is_some_and(|slug| !slug.is_empty())
    };
    list.iter()
        .filter(pick)
        .find(|entry| entry.get("pageType").and_then(Value::as_str) == Some("productHome"))
        .or_else(|| list.iter().find(pick))
        .and_then(|entry| entry.get("pageSlug").and_then(Value::as_str))
        .map(str::to_owned)
}

fn store_url(element: &Value) -> Option<String> {
    let slug = page_slug(element.pointer("/catalogNs/mappings"))
        .or_else(|| page_slug(element.get("offerMappings")))
        .or_else(|| {
            ["productSlug", "urlSlug"].iter().find_map(|key| {
                element
                    .get(*key)
                    .and_then(Value::as_str)
                    .map(|slug| slug.trim_end_matches("/home").to_owned())
            })
        })?;
    // Le slug vient d'un service distant : on n'ouvre que des chemins simples.
    let valid = !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/'));
    valid.then(|| format!("{STORE_PAGE}{slug}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!({"data": {"Catalog": {"searchStore": {"elements": [
            {"title": "Other Game", "description": "x", "customAttributes": []},
            {
                "title": "Cool Game: Edition!",
                "description": " A short pitch. ",
                "effectiveDate": "2021-05-20T15:00:00.000Z",
                "seller": {"name": "Seller Inc"},
                "productSlug": "cool-game/home",
                "customAttributes": [
                    {"key": "developerName", "value": "Dev Studio"},
                    {"key": "publisherName", "value": "Big Publisher"}
                ],
                "catalogNs": {"mappings": [{"pageSlug": "cool-game", "pageType": "productHome"}]},
                "offerMappings": []
            }
        ]}}}})
    }

    #[test]
    fn matches_by_normalised_title_and_reads_attributes() {
        let info = parse(&sample(), "cool game edition").unwrap();
        assert_eq!(info.developer, "Dev Studio");
        assert_eq!(info.publisher, "Big Publisher");
        assert_eq!(info.description, "A short pitch.");
        assert_eq!(info.release_date, "2021-05-20T15:00:00.000Z");
        assert_eq!(info.url, "https://store.epicgames.com/p/cool-game");
    }

    #[test]
    fn placeholder_date_and_missing_publisher_fall_back() {
        let data = json!({"data": {"Catalog": {"searchStore": {"elements": [{
            "title": "Soon",
            "effectiveDate": "2099-01-20T00:00:00.000Z",
            "seller": {"name": "Seller Inc"},
            "productSlug": "soon/home",
            "customAttributes": []
        }]}}}});
        let info = parse(&data, "Soon").unwrap();
        assert_eq!(info.publisher, "Seller Inc");
        assert!(info.release_date.is_empty());
        assert_eq!(info.url, "https://store.epicgames.com/p/soon");
    }

    #[test]
    fn no_match_or_errors_give_none() {
        assert!(parse(&sample(), "Unknown").is_none());
        assert!(parse(&json!({"errors": [{"message": "x"}], "data": null}), "x").is_none());
    }

    #[test]
    fn edition_titles_match_by_prefix() {
        let data = json!({"data": {"Catalog": {"searchStore": {"elements": [
            {"title": "Cool Game Deluxe Edition", "productSlug": "deluxe", "customAttributes": []},
            {"title": "Cool Game Standard", "productSlug": "standard", "customAttributes": []}
        ]}}}});
        let info = parse(&data, "Cool Game").unwrap();
        assert_eq!(info.url, "https://store.epicgames.com/p/standard");
    }

    #[test]
    fn unsafe_slugs_are_rejected() {
        let element = json!({"productSlug": "a b?x=1"});
        assert!(store_url(&element).is_none());
    }
}
