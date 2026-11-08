//! Current weather from Open-Meteo, which needs no API key.

use fm_rs::{Tool, ToolOutput};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Shared, arg_str, failure, output};

pub struct Weather {
    shared: Shared,
    web: reqwest::Client,
}

impl Weather {
    pub(super) fn new(shared: Shared, web: reqwest::Client) -> Self {
        Self { shared, web }
    }
}

impl Tool for Weather {
    fn name(&self) -> &str {
        "getWeather"
    }

    fn description(&self) -> &str {
        "Returns the current weather for a city: temperature, conditions, humidity, and wind. \
         Use it whenever someone asks about the weather or temperature somewhere."
    }

    fn arguments_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "city": { "type": "string", "description": "City name, e.g. 'Seoul' or 'Paris'" }
            }
        })
    }

    fn call(&self, arguments: Value) -> fm_rs::Result<ToolOutput> {
        let Some(city) = arg_str(&arguments, "city") else {
            return failure("missing the \"city\" argument");
        };
        // The geocoder matches names only, so drop any ", Country" suffix.
        let city = city.split(',').next().unwrap_or(city).trim().to_string();
        self.shared.record(format!("Checked the weather in {city}"));

        match self.shared.block_on(lookup(&self.web, &city)) {
            Ok(Ok(Some(report))) => output(report),
            Ok(Ok(None)) => failure(format!("couldn't find a city called {city}")),
            Ok(Err(e)) => failure(format!("the weather service failed: {e}")),
            Err(e) => failure(e),
        }
    }
}

#[derive(Deserialize)]
struct Geocoding {
    #[serde(default)]
    results: Vec<Place>,
}

#[derive(Deserialize)]
struct Place {
    name: String,
    country: Option<String>,
    latitude: f64,
    longitude: f64,
}

#[derive(Deserialize)]
struct Forecast {
    current: Current,
}

#[derive(Deserialize)]
struct Current {
    temperature_2m: f64,
    apparent_temperature: f64,
    relative_humidity_2m: f64,
    wind_speed_10m: f64,
    weather_code: u32,
}

async fn lookup(web: &reqwest::Client, city: &str) -> reqwest::Result<Option<String>> {
    let geocoding: Geocoding = web
        .get("https://geocoding-api.open-meteo.com/v1/search")
        .query(&[("name", city), ("count", "1")])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let Some(place) = geocoding.results.into_iter().next() else {
        return Ok(None);
    };

    let forecast: Forecast = web
        .get("https://api.open-meteo.com/v1/forecast")
        .query(&[
            ("latitude", place.latitude.to_string()),
            ("longitude", place.longitude.to_string()),
            (
                "current",
                "temperature_2m,apparent_temperature,relative_humidity_2m,wind_speed_10m,weather_code"
                    .to_string(),
            ),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let now = forecast.current;
    let location = match place.country {
        Some(country) => format!("{}, {country}", place.name),
        None => place.name,
    };
    Ok(Some(format!(
        "{location}: {}, {:.0}°C ({:.0}°F), feels like {:.0}°C. Humidity {:.0}%, wind {:.0} km/h.",
        describe(now.weather_code),
        now.temperature_2m,
        now.temperature_2m * 9.0 / 5.0 + 32.0,
        now.apparent_temperature,
        now.relative_humidity_2m,
        now.wind_speed_10m,
    )))
}

/// WMO weather interpretation codes, as used by Open-Meteo.
fn describe(code: u32) -> &'static str {
    match code {
        0 => "clear sky",
        1 => "mainly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 | 48 => "fog",
        51 | 53 | 55 => "drizzle",
        56 | 57 => "freezing drizzle",
        61 | 63 | 65 => "rain",
        66 | 67 => "freezing rain",
        71 | 73 | 75 | 77 => "snow",
        80..=82 => "rain showers",
        85 | 86 => "snow showers",
        95 => "thunderstorm",
        96 | 99 => "thunderstorm with hail",
        _ => "unknown conditions",
    }
}
