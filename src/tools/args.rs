//! Reading the arguments the model passed to a tool.
//!
//! The on-device model often mislabels arguments; it has been seen passing
//! the tool's own name as the argument name. So when the expected name is
//! missing, [`Args`] falls back to the only remaining value of the right
//! type. Each value is used at most once, so reading the delay of a
//! reminder first keeps it from also being read as the reminder's text.

use std::cell::RefCell;
use std::time::Duration;

use serde_json::{Map, Value};

use super::ToolError;

pub struct Args<'a> {
    values: Option<&'a Map<String, Value>>,
    used: RefCell<Vec<&'a str>>,
}

impl<'a> Args<'a> {
    pub fn new(arguments: &'a Value) -> Self {
        Self {
            values: arguments.as_object(),
            used: RefCell::new(Vec::new()),
        }
    }

    /// A non-blank text argument.
    pub fn text(&self, name: &str) -> Option<&'a str> {
        self.take(&[name], as_text)
    }

    pub fn require_text(&self, name: &str) -> Result<&'a str, ToolError> {
        self.text(name).ok_or_else(|| ToolError::missing(name))
    }

    /// A whole-number argument. The model sometimes sends numbers as
    /// strings or floats, so all three are accepted.
    pub fn integer(&self, name: &str) -> Option<u64> {
        self.take(&[name], as_integer)
    }

    /// A positive number, such as an amount of money. Accepts "1,500" and
    /// "1500.50" as well as JSON numbers.
    pub fn number(&self, name: &str) -> Option<f64> {
        self.take(&[name], as_number)
    }

    /// A length of time such as "2 hours" (see [`parse_duration`]). Since
    /// the model is especially loose with these, any of `names` is tried.
    pub fn duration(&self, names: &[&str]) -> Option<Duration> {
        self.take(names, as_duration)
    }

    /// The first of `names` whose value `parse` accepts, or else the only
    /// unused value it accepts.
    fn take<T>(&self, names: &[&str], parse: impl Fn(&'a Value) -> Option<T>) -> Option<T> {
        let values = self.values?;
        let mut used = self.used.borrow_mut();
        let unused = || {
            values
                .iter()
                .filter(|(key, _)| !used.contains(&key.as_str()))
        };

        let named = names.iter().find_map(|name| {
            let (key, value) = unused().find(|(key, _)| key == name)?;
            Some((key, parse(value)?))
        });
        let found = named.or_else(|| {
            let mut candidates = unused().filter_map(|(key, value)| Some((key, parse(value)?)));
            let only = candidates.next()?;
            candidates.next().is_none().then_some(only)
        });
        found.map(|(key, value)| {
            used.push(key.as_str());
            value
        })
    }
}

fn as_text(value: &Value) -> Option<&str> {
    value.as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn as_integer(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| {
            value
                .as_f64()
                .filter(|f| *f >= 0.0)
                .map(|f| f.round() as u64)
        })
        .or_else(|| value.as_str()?.trim().parse().ok())
}

fn as_number(value: &Value) -> Option<f64> {
    let number = match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().replace(',', "").parse().ok(),
        _ => None,
    };
    number.filter(|n| n.is_finite() && *n > 0.0)
}

fn as_duration(value: &Value) -> Option<Duration> {
    match value {
        // A bare number is minutes.
        Value::Number(n) => n
            .as_f64()
            .filter(|m| *m > 0.0)
            .map(|m| Duration::from_secs_f64(m * 60.0)),
        Value::String(s) => parse_duration(s),
        _ => None,
    }
}

/// Parses durations like "90", "2 hours", "1h30m", "1 day and 3 hours", or
/// "in 20 min". A number without a unit is minutes.
pub fn parse_duration(text: &str) -> Option<Duration> {
    let text = text.trim().to_lowercase();
    let text = text.strip_prefix("in ").unwrap_or(&text);
    let mut total_minutes = 0.0;
    let mut found = false;
    let mut chars = text.chars().peekable();

    while chars.peek().is_some() {
        // Skip to the next number, ignoring words like "and".
        while chars
            .next_if(|c| !c.is_ascii_digit() && *c != '.')
            .is_some()
        {}
        let number: String =
            std::iter::from_fn(|| chars.next_if(|c| c.is_ascii_digit() || *c == '.')).collect();
        if number.is_empty() {
            break;
        }
        let value: f64 = number.parse().ok()?;
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let unit: String = std::iter::from_fn(|| chars.next_if(|c| c.is_alphabetic())).collect();
        let minutes_per_unit = match unit.as_str() {
            "" | "m" | "min" | "mins" | "minute" | "minutes" => 1.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 60.0,
            "d" | "day" | "days" => 24.0 * 60.0,
            "w" | "wk" | "week" | "weeks" => 7.0 * 24.0 * 60.0,
            "s" | "sec" | "secs" | "second" | "seconds" => 1.0 / 60.0,
            _ => return None,
        };
        total_minutes += value * minutes_per_unit;
        found = true;
    }
    (found && total_minutes > 0.0).then(|| Duration::from_secs_f64(total_minutes * 60.0))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const MIN: u64 = 60;

    #[test]
    fn reads_numbers_in_any_shape() {
        for value in [json!(5), json!(5.4), json!(" 5 ")] {
            let args = json!({ "n": value });
            assert_eq!(Args::new(&args).integer("n"), Some(5), "{value}");
        }
        let args = json!({ "n": -3, "m": "nope" });
        assert_eq!(Args::new(&args).integer("n"), None);
    }

    #[test]
    fn reads_amounts() {
        for (value, expected) in [
            (json!(12.5), 12.5),
            (json!("1,500"), 1500.0),
            (json!(" 3 "), 3.0),
        ] {
            let args = json!({ "amount": value });
            assert_eq!(Args::new(&args).number("amount"), Some(expected), "{value}");
        }
        for value in [json!(0), json!(-2), json!("lots")] {
            let args = json!({ "amount": value });
            assert_eq!(Args::new(&args).number("amount"), None, "{value}");
        }
    }

    #[test]
    fn blank_text_counts_as_missing() {
        let args = json!({ "a": "  hi ", "b": "   " });
        let args = Args::new(&args);
        assert_eq!(args.text("a"), Some("hi"));
        assert_eq!(args.text("b"), None);
        assert_eq!(args.require_text("b"), Err(ToolError::missing("b")));
    }

    #[test]
    fn falls_back_to_the_only_value_of_the_right_type() {
        // Seen from the real model: the tool name used as the argument name.
        let args = json!({ "getWeather": "Seoul" });
        assert_eq!(Args::new(&args).text("city"), Some("Seoul"));

        let args = json!({ "delay": "30", "text": "stretch" });
        assert_eq!(Args::new(&args).integer("minutes"), Some(30));
    }

    #[test]
    fn does_not_guess_between_several_values() {
        let args = json!({ "x": "Seoul", "y": "Paris" });
        assert_eq!(Args::new(&args).text("city"), None);
        let args = json!({ "x": 1, "y": 2 });
        assert_eq!(Args::new(&args).integer("minutes"), None);
    }

    #[test]
    fn each_value_is_used_once() {
        // Shapes seen from the real model for setReminder.
        let args = json!({ "setReminder": "30 min", "text": "call mom" });
        let args = Args::new(&args);
        assert_eq!(
            args.duration(&["delay"]),
            Some(Duration::from_secs(30 * MIN))
        );
        assert_eq!(args.text("message"), Some("call mom"));

        let args = json!({ "minutes": 120, "message": "stretch" });
        let args = Args::new(&args);
        assert_eq!(
            args.duration(&["delay", "minutes"]),
            Some(Duration::from_secs(120 * MIN))
        );
        assert_eq!(args.text("message"), Some("stretch"));
    }

    #[test]
    fn missing_duration_is_none() {
        let args = json!({ "message": "stretch" });
        assert_eq!(Args::new(&args).duration(&["delay"]), None);
    }

    #[test]
    fn non_object_arguments_have_no_values() {
        let args = json!("Seoul");
        assert_eq!(Args::new(&args).text("city"), None);
    }

    #[test]
    fn parses_natural_durations() {
        let cases = [
            ("90", 90 * MIN),
            ("20 minutes", 20 * MIN),
            ("2 hours", 120 * MIN),
            ("in 2 hours", 120 * MIN),
            ("1h30m", 90 * MIN),
            ("1 day and 3 hours", 27 * 60 * MIN),
            ("1.5 hours", 90 * MIN),
            ("2 Weeks", 14 * 24 * 60 * MIN),
        ];
        for (text, secs) in cases {
            assert_eq!(
                parse_duration(text),
                Some(Duration::from_secs(secs)),
                "{text}"
            );
        }
    }

    #[test]
    fn rejects_things_that_are_not_durations() {
        for text in ["", "stretch", "soon", "2 fortnights", "0 minutes"] {
            assert_eq!(parse_duration(text), None, "{text}");
        }
    }
}
