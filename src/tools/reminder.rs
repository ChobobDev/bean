use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Local};
use fm_rs::{Tool, ToolOutput};
use serde_json::{Value, json};
use serenity::all::ChannelId;

use super::{Shared, failure, output};
use crate::reminders::Reminders;

pub struct SetReminder {
    shared: Shared,
    channel_id: ChannelId,
    reminders: Arc<Reminders>,
}

impl SetReminder {
    pub(super) fn new(shared: Shared, channel_id: ChannelId, reminders: Arc<Reminders>) -> Self {
        Self { shared, channel_id, reminders }
    }
}

impl Tool for SetReminder {
    fn name(&self) -> &str {
        "setReminder"
    }

    fn description(&self) -> &str {
        "Schedules a reminder that pings the person you're talking to in this channel after a \
         delay. Use it whenever someone asks to be reminded of something. Always pass both \
         'delay' and 'message'."
    }

    fn arguments_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "delay": {
                    "type": "string",
                    "description": "How long from now, e.g. '20 minutes', '2 hours', or '1 day' (at most 7 days)"
                },
                "message": { "type": "string", "description": "What to remind them about" }
            }
        })
    }

    fn call(&self, arguments: Value) -> fm_rs::Result<ToolOutput> {
        let Some(user_id) = self.shared.author_id() else {
            return failure("I don't know who to remind");
        };
        let (delay, message) = read_arguments(&arguments);
        let Some(delay) = delay else {
            return failure("missing the \"delay\" argument, e.g. \"2 hours\"");
        };
        let Some(message) = message else {
            return failure("missing the \"message\" argument");
        };

        match self.reminders.add(self.channel_id.get(), user_id, delay, message) {
            Ok(reminder) => {
                let due = format_due(reminder.due_at);
                self.shared.record(format!("Set a reminder for {due}"));
                output(format!("Reminder set for {due}: {}", reminder.text))
            }
            Err(e) => failure(e),
        }
    }
}

pub struct ListReminders {
    shared: Shared,
    reminders: Arc<Reminders>,
}

impl ListReminders {
    pub(super) fn new(shared: Shared, reminders: Arc<Reminders>) -> Self {
        Self { shared, reminders }
    }
}

impl Tool for ListReminders {
    fn name(&self) -> &str {
        "listReminders"
    }

    fn description(&self) -> &str {
        "Lists the reminders the person you're talking to has pending. Use it when they ask what \
         reminders they have."
    }

    fn arguments_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    fn call(&self, _arguments: Value) -> fm_rs::Result<ToolOutput> {
        let Some(user_id) = self.shared.author_id() else {
            return failure("I don't know whose reminders to list");
        };
        self.shared.record("Checked reminders");
        let pending = self.reminders.pending_for(user_id);
        if pending.is_empty() {
            return output("No reminders pending.");
        }
        let lines: Vec<_> = pending
            .iter()
            .map(|r| format!("- {}: {}", format_due(r.due_at), r.text))
            .collect();
        output(lines.join("\n"))
    }
}

/// Reads the delay and message. The on-device model often renames or
/// mislabels arguments, so the delay is the first value that parses as a
/// duration and the message is whatever text is left.
fn read_arguments(arguments: &Value) -> (Option<Duration>, Option<&str>) {
    let Some(args) = arguments.as_object() else {
        return (None, None);
    };
    let delay_key = ["delay", "minutes", "time", "duration", "in"]
        .into_iter()
        .find(|key| args.get(*key).and_then(parse_duration_value).is_some())
        .or_else(|| {
            args.iter()
                .find(|(key, value)| *key != "message" && parse_duration_value(value).is_some())
                .map(|(key, _)| key.as_str())
        });
    let delay = delay_key.and_then(|key| parse_duration_value(&args[key]));

    let message = args.get("message").and_then(text).or_else(|| {
        let mut rest = args
            .iter()
            .filter(|(key, _)| Some(key.as_str()) != delay_key)
            .filter_map(|(_, value)| text(value));
        let only = rest.next()?;
        rest.next().is_none().then_some(only)
    });
    (delay, message)
}

fn text(value: &Value) -> Option<&str> {
    value.as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn parse_duration_value(value: &Value) -> Option<Duration> {
    match value {
        // A bare number is minutes.
        Value::Number(n) => n.as_f64().filter(|m| *m > 0.0).map(|m| Duration::from_secs_f64(m * 60.0)),
        Value::String(s) => parse_duration(s),
        _ => None,
    }
}

/// Parses durations like "90", "2 hours", "1h30m", "1 day and 3 hours", or
/// "in 20 min". A number without a unit is minutes.
pub fn parse_duration(text: &str) -> Option<Duration> {
    let text = text.trim().to_lowercase();
    let text = text.strip_prefix("in ").unwrap_or(&text);
    let mut total = 0.0;
    let mut found = false;
    let mut chars = text.chars().peekable();

    while chars.peek().is_some() {
        // Skip to the next number, ignoring words like "and".
        while chars.peek().is_some_and(|c| !c.is_ascii_digit() && *c != '.') {
            chars.next();
        }
        let number: String = std::iter::from_fn(|| chars.next_if(|c| c.is_ascii_digit() || *c == '.')).collect();
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
        total += value * minutes_per_unit;
        found = true;
    }
    (found && total > 0.0).then(|| Duration::from_secs_f64(total * 60.0))
}

fn format_due(unix_secs: u64) -> String {
    DateTime::from_timestamp(unix_secs as i64, 0)
        .map(|t| t.with_timezone(&Local).format("%a %-d %b, %H:%M").to_string())
        .unwrap_or_else(|| "an unknown time".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: u64 = 60;

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
            assert_eq!(parse_duration(text), Some(Duration::from_secs(secs)), "{text}");
        }
    }

    #[test]
    fn rejects_things_that_are_not_durations() {
        for text in ["", "stretch", "soon", "2 fortnights", "0 minutes"] {
            assert_eq!(parse_duration(text), None, "{text}");
        }
    }

    #[test]
    fn reads_well_formed_arguments() {
        let args = json!({ "delay": "2 hours", "message": "stretch" });
        let (delay, message) = read_arguments(&args);
        assert_eq!(delay, Some(Duration::from_secs(120 * MIN)));
        assert_eq!(message, Some("stretch"));
    }

    #[test]
    fn reads_mislabeled_arguments() {
        // Shapes seen from the real model.
        let args = json!({ "minutes": 120, "message": "stretch" });
        let (delay, message) = read_arguments(&args);
        assert_eq!(delay, Some(Duration::from_secs(120 * MIN)));
        assert_eq!(message, Some("stretch"));

        let args = json!({ "setReminder": "30 min", "text": "call mom" });
        let (delay, message) = read_arguments(&args);
        assert_eq!(delay, Some(Duration::from_secs(30 * MIN)));
        assert_eq!(message, Some("call mom"));
    }

    #[test]
    fn reports_missing_delay() {
        let args = json!({ "message": "stretch" });
        let (delay, message) = read_arguments(&args);
        assert_eq!(delay, None);
        assert_eq!(message, Some("stretch"));
    }
}
