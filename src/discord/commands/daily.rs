//! `/daily`: a quote and a song every morning in this channel.

use chrono::{Local, NaiveTime};
use serenity::all::{
    CommandInteraction, CommandOptionType, Context, CreateCommand, CreateCommandOption,
    Permissions, ResolvedOption, ResolvedValue,
};
use tracing::warn;

use crate::daily::{DEFAULT_TIME, Subscription, parse_time};
use crate::discord::Handler;

pub(super) fn definition(name: &str) -> CreateCommand {
    let start = CreateCommandOption::new(
        CommandOptionType::SubCommand,
        "start",
        "Post a quote and a song here every day",
    )
    .add_sub_option(CreateCommandOption::new(
        CommandOptionType::String,
        "time",
        "When to post, e.g. 08:00 or 7:30pm (default 08:00, this Mac's time zone)",
    ));
    let subcommand = |name, description| {
        CreateCommandOption::new(CommandOptionType::SubCommand, name, description)
    };
    CreateCommand::new(name)
        .description("A quote and a song every morning")
        // Posting in a channel every day is for people who manage it.
        .default_member_permissions(Permissions::MANAGE_CHANNELS)
        .add_option(start)
        .add_option(subcommand("stop", "Stop the daily posts here"))
        .add_option(subcommand(
            "status",
            "Show when the daily post goes out here",
        ))
        .add_option(subcommand(
            "preview",
            "Post today's quote and song here now",
        ))
}

impl Handler {
    pub(super) async fn run_daily_command(
        &self,
        ctx: &Context,
        interaction: &CommandInteraction,
    ) -> String {
        let daily = &self.services.daily;
        let channel_id = interaction.channel_id.get();
        let options = interaction.data.options();
        let Some(ResolvedOption {
            name,
            value: ResolvedValue::SubCommand(sub_options),
            ..
        }) = options.first()
        else {
            return "Pick one: start, stop, status, or preview.".into();
        };

        let saved = |result: std::io::Result<String>| {
            result.unwrap_or_else(|e| {
                warn!("failed to save the daily schedule: {e}");
                "I couldn't save that. Check the logs.".into()
            })
        };
        match *name {
            "start" => {
                let time = match time_option(sub_options) {
                    Some(text) => match parse_time(text) {
                        Some(time) => time,
                        None => {
                            return format!(
                                "I couldn't read {text:?} as a time. Try something like 08:00 or 7:30pm."
                            );
                        }
                    },
                    None => DEFAULT_TIME,
                };
                saved(
                    daily
                        .subscribe(channel_id, time)
                        .map(|s| started(&s, daily.has_songs())),
                )
            }
            "stop" => saved(daily.unsubscribe(channel_id).map(|removed| {
                if removed {
                    "Stopped. No more daily posts here.".into()
                } else {
                    "There were no daily posts set up here.".into()
                }
            })),
            "status" => match daily.subscription(channel_id) {
                Some(subscription) => format!(
                    "The daily post goes out here at {}.",
                    clock(subscription.time)
                ),
                None => "There are no daily posts here. Use `/daily start` to set one up.".into(),
            },
            "preview" => match daily.post(&ctx.http, channel_id).await {
                Ok(()) => "Posted today's quote here.".into(),
                Err(e) => format!("I couldn't post here: {e}"),
            },
            other => format!("Unknown option {other}."),
        }
    }
}

fn time_option<'a>(options: &'a [ResolvedOption<'a>]) -> Option<&'a str> {
    options.iter().find_map(|option| match option.value {
        ResolvedValue::String(text) if option.name == "time" => Some(text),
        _ => None,
    })
}

fn started(subscription: &Subscription, has_songs: bool) -> String {
    let what = if has_songs {
        "a quote and a song"
    } else {
        "a quote"
    };
    let first = if subscription.last_sent.is_some() {
        "tomorrow"
    } else {
        "today"
    };
    let mut reply = format!(
        "Done! I'll post {what} here every day at {}. The first one is {first}.",
        clock(subscription.time)
    );
    if !has_songs {
        reply.push_str("\n-# Songs are off because Spotify isn't set up.");
    }
    reply
}

/// "08:00 (UTC+09:00)", in the Mac's time zone.
fn clock(time: NaiveTime) -> String {
    format!(
        "{} ({})",
        time.format("%H:%M"),
        Local::now().format("UTC%:z")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subscription(last_sent: Option<&str>) -> Subscription {
        Subscription {
            channel_id: 1,
            time: DEFAULT_TIME,
            last_sent: last_sent.map(|d| d.parse().unwrap()),
        }
    }

    #[test]
    fn says_when_the_first_post_is() {
        assert!(started(&subscription(None), true).contains("first one is today"));
        let reply = started(&subscription(Some("2026-11-09")), true);
        assert!(reply.contains("a quote and a song"), "{reply}");
        assert!(reply.contains("first one is tomorrow"), "{reply}");
    }

    #[test]
    fn explains_missing_songs() {
        let reply = started(&subscription(None), false);
        assert!(reply.contains("post a quote here"), "{reply}");
        assert!(reply.contains("Spotify isn't set up"), "{reply}");
    }
}
