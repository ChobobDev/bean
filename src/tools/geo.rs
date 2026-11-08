//! Finding places by name, via Open-Meteo's geocoder (no API key).

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Place {
    pub name: String,
    pub country: Option<String>,
    pub latitude: f64,
    pub longitude: f64,
    /// IANA time zone, e.g. "America/New_York".
    pub timezone: Option<String>,
}

impl Place {
    /// "Seoul, South Korea", or just the name if the country is unknown.
    pub fn label(&self) -> String {
        match &self.country {
            Some(country) => format!("{}, {country}", self.name),
            None => self.name.clone(),
        }
    }
}

#[derive(Deserialize)]
struct Geocoding {
    #[serde(default)]
    results: Vec<Place>,
}

/// Looks up the best match for `name`. The geocoder matches place names
/// only, so a ", Country" suffix is dropped first.
pub async fn find_place(web: &reqwest::Client, name: &str) -> reqwest::Result<Option<Place>> {
    let name = place_name(name);
    let geocoding: Geocoding = web
        .get("https://geocoding-api.open-meteo.com/v1/search")
        .query(&[("name", name), ("count", "1")])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(geocoding.results.into_iter().next())
}

/// The part of `name` the geocoder understands: "Paris, France" → "Paris".
pub fn place_name(name: &str) -> &str {
    name.split(',').next().unwrap_or(name).trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_country_suffix() {
        assert_eq!(place_name("Paris, France"), "Paris");
        assert_eq!(place_name("  Seoul "), "Seoul");
    }

    #[test]
    fn parses_geocoder_results() {
        let json = r#"{"results":[{"id":5128581,"name":"New York","latitude":40.71,"longitude":-74.0,"timezone":"America/New_York","country":"United States"}]}"#;
        let place = serde_json::from_str::<Geocoding>(json)
            .unwrap()
            .results
            .remove(0);
        assert_eq!(place.label(), "New York, United States");
        assert_eq!(place.timezone.as_deref(), Some("America/New_York"));
    }

    #[test]
    fn no_results_is_empty() {
        let geocoding: Geocoding = serde_json::from_str(r#"{"generationtime_ms":0.5}"#).unwrap();
        assert!(geocoding.results.is_empty());
    }
}
