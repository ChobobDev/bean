//! Current weather from Open-Meteo, which needs no API key.

use serde::Deserialize;

use super::geo::{self, Place};
use super::{Args, BeanTool, Param, ToolContext, ToolError, ToolResult};

pub struct Weather;

impl BeanTool for Weather {
    fn name(&self) -> &'static str {
        "getWeather"
    }

    fn description(&self) -> &'static str {
        "Returns the current weather for a city: temperature, conditions, humidity, and wind. \
         Use it whenever someone asks about the weather or temperature somewhere."
    }

    fn params(&self) -> &'static [Param] {
        const PARAMS: &[Param] = &[Param::text("city", "City name, e.g. 'Seoul' or 'Paris'")];
        PARAMS
    }

    fn run(&self, args: &Args, ctx: &ToolContext) -> ToolResult {
        let city = geo::place_name(args.require_text("city")?);
        ctx.record(format!("Checked the weather in {city}"));

        ctx.block_on(lookup(ctx.web(), city))?
            .map_err(|e| ToolError::new(format!("the weather service failed: {e}")))?
            .ok_or_else(|| ToolError::new(format!("couldn't find a city called {city}")))
    }
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
    let Some(place) = geo::find_place(web, city).await? else {
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

    Ok(Some(report(&place, &forecast.current)))
}

fn report(place: &Place, now: &Current) -> String {
    format!(
        "{}: {}, {:.0}°C ({:.0}°F), feels like {:.0}°C. Humidity {:.0}%, wind {:.0} km/h.",
        place.label(),
        describe(now.weather_code),
        now.temperature_2m,
        now.temperature_2m * 9.0 / 5.0 + 32.0,
        now.apparent_temperature,
        now.relative_humidity_2m,
        now.wind_speed_10m,
    )
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
