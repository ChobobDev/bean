//! The current time here or anywhere. The model has no clock, so it needs
//! this for anything date-related.

use chrono::{DateTime, Local, TimeZone, Utc};
use chrono_tz::Tz;

use super::geo;
use super::{Args, BeanTool, Param, ToolContext, ToolError, ToolResult};

const FORMAT: &str = "%A, %-d %B %Y, %H:%M (UTC%:z)";

pub struct CurrentTime;

impl BeanTool for CurrentTime {
    fn name(&self) -> &'static str {
        "getCurrentTime"
    }

    fn description(&self) -> &'static str {
        "Returns the current date and time, here or in another city. Use it whenever the answer \
         depends on today's date or the time, such as 'what day is it', 'how long until Friday', \
         or 'what time is it in Tokyo'."
    }

    fn params(&self) -> &'static [Param] {
        const PARAMS: &[Param] = &[Param::text(
            "city",
            "Only for another place's time, e.g. 'New York'. Leave it out for the time here.",
        )];
        PARAMS
    }

    fn run(&self, args: &Args, ctx: &ToolContext) -> ToolResult {
        let now = Utc::now();
        let Some(city) = args
            .text("city")
            .map(geo::place_name)
            .filter(|c| !means_here(c))
        else {
            ctx.record("Checked the time");
            return Ok(local_time(now));
        };

        // IANA names like "Asia/Tokyo" or "UTC" work without a lookup.
        if let Ok(zone) = city.parse::<Tz>() {
            ctx.record(format!("Checked the time in {city}"));
            return Ok(format!("{}: {}", zone.name(), time_in(zone, now)));
        }

        let place = ctx
            .block_on(geo::find_place(ctx.web(), city))?
            .map_err(|e| ToolError::new(format!("the place lookup failed: {e}")))?;
        let zone = place
            .as_ref()
            .and_then(|p| p.timezone.as_deref())
            .and_then(|zone| zone.parse::<Tz>().ok());
        match (place, zone) {
            (Some(place), Some(zone)) => {
                ctx.record(format!("Checked the time in {city}"));
                Ok(format!("{}: {}", place.label(), time_in(zone, now)))
            }
            // Still give the model the local time to work with.
            _ => {
                ctx.record("Checked the time");
                Ok(format!(
                    "Couldn't find a place called {city}. The local time here is {}.",
                    local_time(now)
                ))
            }
        }
    }
}

/// Whether `city` is really a way of saying "here and now". The model
/// sometimes passes words like "today" as the city.
fn means_here(city: &str) -> bool {
    const WORDS: &[&str] = &[
        "today",
        "now",
        "here",
        "local",
        "current",
        "currently",
        "date",
        "time",
        "tomorrow",
        "yesterday",
        "my location",
        "this week",
    ];
    WORDS.contains(&city.to_lowercase().as_str())
}

fn local_time(now: DateTime<Utc>) -> String {
    now.with_timezone(&Local).format(FORMAT).to_string()
}

fn time_in(zone: impl TimeZone<Offset: std::fmt::Display>, now: DateTime<Utc>) -> String {
    now.with_timezone(&zone).format(FORMAT).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_time_in_a_zone() {
        let now = Utc.with_ymd_and_hms(2026, 11, 8, 13, 5, 0).unwrap();
        let zone: Tz = "America/New_York".parse().unwrap();
        assert_eq!(
            time_in(zone, now),
            "Sunday, 8 November 2026, 08:05 (UTC-05:00)"
        );
        let zone: Tz = "Asia/Seoul".parse().unwrap();
        assert_eq!(
            time_in(zone, now),
            "Sunday, 8 November 2026, 22:05 (UTC+09:00)"
        );
    }

    #[test]
    fn treats_time_words_as_here() {
        for word in ["today", "Now", "local", "current"] {
            assert!(means_here(word), "{word}");
        }
        assert!(!means_here("New York"));
    }

    #[test]
    fn recognizes_iana_names() {
        assert!("Asia/Tokyo".parse::<Tz>().is_ok());
        assert!("UTC".parse::<Tz>().is_ok());
        assert!("New York".parse::<Tz>().is_err());
    }
}
