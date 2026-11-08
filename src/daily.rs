//! A quote and a song every morning, in channels that ask for one.
//!
//! The quote is ZenQuotes' quote of the day. The song is a random Spotify
//! track in a genre the model picks to match the quote's mood. Times are in
//! the Mac's own time zone.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{Local, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta};
use serde::{Deserialize, Serialize};
use serenity::all::{ChannelId, CreateAllowedMentions, CreateMessage, Http};
use tokio::runtime::Handle;
use tracing::{info, warn};

use crate::ai::Engine;
use crate::spotify::{Spotify, Track};
use crate::{storage, ticker};

/// When posts go out unless the channel picks another time.
pub const DEFAULT_TIME: NaiveTime = match NaiveTime::from_hms_opt(8, 0, 0) {
    Some(time) => time,
    None => unreachable!(),
};

/// If the Mac was asleep or Bean was offline at post time, it still posts
/// when it's back within this window, and skips the day otherwise.
const CATCH_UP_HOURS: i64 = 6;

/// Genres the model may pick for the song. These are names Spotify's
/// `genre:` search filter understands.
const GENRES: &[&str] = &[
    "jazz",
    "soul",
    "acoustic",
    "indie",
    "classical",
    "folk",
    "pop",
    "k-pop",
    "ambient",
    "blues",
    "funk",
    "piano",
    "r&b",
    "rock",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    pub channel_id: u64,
    pub time: NaiveTime,
    /// The last day a post went out (or was skipped).
    pub last_sent: Option<NaiveDate>,
}

impl Subscription {
    /// Whether a post should go out at `now`.
    fn is_due(&self, now: NaiveDateTime) -> bool {
        let scheduled = now.date().and_time(self.time);
        let caught_up = now - scheduled < TimeDelta::hours(CATCH_UP_HOURS);
        now >= scheduled && caught_up && self.last_sent != Some(now.date())
    }
}

/// Which channels get posts, and when. Saved to disk.
struct Schedule {
    path: PathBuf,
    subscriptions: Mutex<Vec<Subscription>>,
}

impl Schedule {
    fn open(path: PathBuf) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let subscriptions = match storage::read_json(&path) {
            Ok(subscriptions) => subscriptions.unwrap_or_default(),
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                warn!(path = %path.display(), "ignoring corrupt daily schedule: {e}");
                Vec::new()
            }
            Err(e) => return Err(e),
        };
        Ok(Self {
            path,
            subscriptions: Mutex::new(subscriptions),
        })
    }

    /// Posts to `channel_id` every day at `time`, replacing any earlier
    /// schedule there. If `time` has already passed today, the first post
    /// is tomorrow.
    fn subscribe(
        &self,
        channel_id: u64,
        time: NaiveTime,
        now: NaiveDateTime,
    ) -> io::Result<Subscription> {
        let subscription = Subscription {
            channel_id,
            time,
            last_sent: (now.time() >= time).then(|| now.date()),
        };
        self.update(|subscriptions| {
            subscriptions.retain(|s| s.channel_id != channel_id);
            subscriptions.push(subscription.clone());
        })?;
        Ok(subscription)
    }

    /// Stops posts to `channel_id`. Returns whether there were any.
    fn unsubscribe(&self, channel_id: u64) -> io::Result<bool> {
        let mut removed = false;
        self.update(|subscriptions| {
            let before = subscriptions.len();
            subscriptions.retain(|s| s.channel_id != channel_id);
            removed = subscriptions.len() != before;
        })?;
        Ok(removed)
    }

    fn get(&self, channel_id: u64) -> Option<Subscription> {
        self.lock()
            .iter()
            .find(|s| s.channel_id == channel_id)
            .cloned()
    }

    /// Channels due a post at `now`. They're marked as sent right away, so
    /// a channel Bean can't post to isn't retried every few seconds.
    fn take_due(&self, now: NaiveDateTime) -> Vec<u64> {
        let mut due = Vec::new();
        let result = self.update(|subscriptions| {
            for subscription in subscriptions.iter_mut().filter(|s| s.is_due(now)) {
                subscription.last_sent = Some(now.date());
                due.push(subscription.channel_id);
            }
        });
        if let Err(e) = result {
            warn!("failed to save the daily schedule: {e}");
        }
        due
    }

    fn update(&self, change: impl FnOnce(&mut Vec<Subscription>)) -> io::Result<()> {
        let mut subscriptions = self.lock();
        let before = subscriptions.clone();
        change(&mut subscriptions);
        if *subscriptions == before {
            return Ok(());
        }
        storage::write_json(&self.path, &*subscriptions).inspect_err(|_| *subscriptions = before)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Subscription>> {
        self.subscriptions
            .lock()
            .expect("daily schedule lock poisoned")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Quote {
    #[serde(rename = "q")]
    pub text: String,
    #[serde(rename = "a")]
    pub author: String,
}

/// Fetches the day's quote and picks a song for it.
struct Content {
    web: reqwest::Client,
    spotify: Option<Arc<Spotify>>,
    engine: Arc<Engine>,
    /// Today's quote. ZenQuotes allows only a few requests a minute.
    quote: tokio::sync::Mutex<Option<(NaiveDate, Quote)>>,
}

impl Content {
    async fn post_for(&self, today: NaiveDate) -> String {
        let quote = self.quote(today).await;
        let song = self.song(quote.as_ref()).await;
        format_post(today, quote.as_ref(), song.as_ref())
    }

    async fn quote(&self, today: NaiveDate) -> Option<Quote> {
        let mut cached = self.quote.lock().await;
        if let Some((date, quote)) = cached.as_ref()
            && *date == today
        {
            return Some(quote.clone());
        }
        match fetch_quote(&self.web).await {
            Ok(Some(quote)) => {
                *cached = Some((today, quote.clone()));
                Some(quote)
            }
            Ok(None) => {
                warn!("ZenQuotes returned no quote of the day");
                None
            }
            Err(e) => {
                warn!("couldn't fetch the quote of the day: {e}");
                None
            }
        }
    }

    /// A song whose genre suits `quote`, if Spotify is set up.
    async fn song(&self, quote: Option<&Quote>) -> Option<(&'static str, Track)> {
        let spotify = self.spotify.as_ref()?;
        let genre = match quote {
            Some(quote) => self.genre_for(quote).await,
            None => random_genre(),
        };
        match spotify.random_track(genre).await {
            Ok(Some(track)) => Some((genre, track)),
            Ok(None) => {
                warn!(genre, "Spotify found no songs for the daily post");
                None
            }
            Err(e) => {
                warn!("Spotify search for the daily post failed: {e}");
                None
            }
        }
    }

    /// Asks the model which genre fits the quote's mood.
    async fn genre_for(&self, quote: &Quote) -> &'static str {
        let engine = Arc::clone(&self.engine);
        let prompt = genre_prompt(quote);
        let reply = tokio::task::spawn_blocking(move || engine.ask_once(&prompt)).await;
        match reply {
            Ok(Ok(reply)) => parse_genre(&reply).unwrap_or_else(random_genre),
            Ok(Err(e)) => {
                warn!("couldn't pick a genre for the quote: {e}");
                random_genre()
            }
            Err(e) => {
                warn!("genre task failed: {e}");
                random_genre()
            }
        }
    }
}

async fn fetch_quote(web: &reqwest::Client) -> reqwest::Result<Option<Quote>> {
    let quotes: Vec<Quote> = web
        .get("https://zenquotes.io/api/today")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(quotes.into_iter().next())
}

fn genre_prompt(quote: &Quote) -> String {
    format!(
        "Here is a quote: \"{}\"\n\nWhich one of these music genres best fits its mood? {}. \
         Reply with only the genre.",
        quote.text,
        GENRES.join(", ")
    )
}

/// The first of [`GENRES`] named in `reply`. Longer names are checked
/// first, so "k-pop" isn't read as "pop".
fn parse_genre(reply: &str) -> Option<&'static str> {
    let reply = reply.to_lowercase();
    let mut genres = GENRES.to_vec();
    genres.sort_by_key(|g| std::cmp::Reverse(g.len()));
    genres.into_iter().find(|genre| reply.contains(genre))
}

fn random_genre() -> &'static str {
    GENRES[fastrand::usize(..GENRES.len())]
}

fn format_post(today: NaiveDate, quote: Option<&Quote>, song: Option<&(&str, Track)>) -> String {
    let mut lines = Vec::new();
    match quote {
        Some(quote) => {
            lines.push(format!(
                "☀️ **Good morning!** Here's your quote for {}.",
                today.format("%A, %-d %B")
            ));
            lines.extend(quote.text.lines().map(|line| format!("> {line}")));
            lines.push(format!("> — {}", quote.author));
        }
        None => lines.push("☀️ **Good morning!** I couldn't fetch today's quote.".into()),
    }
    if let Some((genre, track)) = song {
        lines.push(String::new());
        lines.push(format!("🎵 **Today's song** ({genre}): {}", track.title()));
        lines.push(track.url.clone());
    }
    if quote.is_some() {
        // ZenQuotes' free tier requires a link back. The angle brackets
        // stop Discord from showing a preview card for it.
        lines.push("-# Quote from <https://zenquotes.io>".into());
    }
    lines.join("\n")
}

/// Parses times like "8", "08:00", "7:30", "8am", or "7:30 pm".
pub fn parse_time(text: &str) -> Option<NaiveTime> {
    let text = text.trim().to_lowercase().replace(' ', "");
    let (clock, offset) = if let Some(clock) = text.strip_suffix("am") {
        (clock, Some(0))
    } else if let Some(clock) = text.strip_suffix("pm") {
        (clock, Some(12))
    } else {
        (text.as_str(), None)
    };
    let (hour, minute) = clock.split_once(':').unwrap_or((clock, "0"));
    let (mut hour, minute): (u32, u32) = (hour.parse().ok()?, minute.parse().ok()?);
    if let Some(offset) = offset {
        if !(1..=12).contains(&hour) {
            return None;
        }
        hour = hour % 12 + offset;
    }
    NaiveTime::from_hms_opt(hour, minute, 0)
}

/// Daily posts: who gets them, and posting them on time.
pub struct Daily {
    runtime: Handle,
    schedule: Schedule,
    content: Content,
    http: OnceLock<Arc<Http>>,
}

impl Daily {
    pub fn open(
        path: impl Into<PathBuf>,
        runtime: Handle,
        web: reqwest::Client,
        spotify: Option<Arc<Spotify>>,
        engine: Arc<Engine>,
    ) -> io::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            runtime,
            schedule: Schedule::open(path.into())?,
            content: Content {
                web,
                spotify,
                engine,
                quote: tokio::sync::Mutex::new(None),
            },
            http: OnceLock::new(),
        }))
    }

    /// Starts posting on schedule. Calling it again does nothing.
    pub fn start(self: &Arc<Self>, http: Arc<Http>) {
        if self.http.set(http).is_err() {
            return;
        }
        info!(
            channels = self.schedule.lock().len(),
            "daily posts scheduled"
        );
        let this = Arc::clone(self);
        ticker::spawn(&self.runtime, move || {
            let this = Arc::clone(&this);
            async move { this.post_due().await }
        });
    }

    pub fn has_songs(&self) -> bool {
        self.content.spotify.is_some()
    }

    pub fn subscribe(&self, channel_id: u64, time: NaiveTime) -> io::Result<Subscription> {
        self.schedule
            .subscribe(channel_id, time, Local::now().naive_local())
    }

    pub fn unsubscribe(&self, channel_id: u64) -> io::Result<bool> {
        self.schedule.unsubscribe(channel_id)
    }

    pub fn subscription(&self, channel_id: u64) -> Option<Subscription> {
        self.schedule.get(channel_id)
    }

    /// Posts today's quote and song to `channel_id` now.
    pub async fn post(&self, http: &Http, channel_id: u64) -> serenity::Result<()> {
        let post = self.content.post_for(Local::now().date_naive()).await;
        let message = CreateMessage::new()
            .content(post)
            .allowed_mentions(CreateAllowedMentions::new());
        ChannelId::new(channel_id)
            .send_message(http, message)
            .await?;
        Ok(())
    }

    async fn post_due(&self) {
        let Some(http) = self.http.get() else { return };
        for channel_id in self.schedule.take_due(Local::now().naive_local()) {
            match self.post(http, channel_id).await {
                Ok(()) => info!(channel_id, "posted the daily quote"),
                Err(e) => warn!(channel_id, "failed to post the daily quote: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(date: &str, time: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M").unwrap()
    }

    fn time(text: &str) -> NaiveTime {
        NaiveTime::parse_from_str(text, "%H:%M").unwrap()
    }

    fn subscription(last_sent: Option<&str>) -> Subscription {
        Subscription {
            channel_id: 1,
            time: time("08:00"),
            last_sent: last_sent.map(|d| d.parse().unwrap()),
        }
    }

    fn temp_schedule(name: &str) -> Schedule {
        let dir = std::env::temp_dir().join(format!("bean-daily-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Schedule::open(dir.join("daily.json")).unwrap()
    }

    #[test]
    fn parses_times() {
        let cases = [
            ("8", "08:00"),
            ("08:00", "08:00"),
            ("7:30", "07:30"),
            ("8am", "08:00"),
            ("7:30 PM", "19:30"),
            ("12am", "00:00"),
            ("12pm", "12:00"),
            ("23:59", "23:59"),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_time(text), Some(time(expected)), "{text}");
        }
        for text in ["", "noon", "25:00", "8:61", "13pm", "0am"] {
            assert_eq!(parse_time(text), None, "{text}");
        }
    }

    #[test]
    fn is_due_once_a_day_after_the_time() {
        let fresh = subscription(None);
        assert!(!fresh.is_due(at("2026-11-09", "07:59")));
        assert!(fresh.is_due(at("2026-11-09", "08:00")));
        assert!(fresh.is_due(at("2026-11-09", "09:30")));

        let sent_today = subscription(Some("2026-11-09"));
        assert!(!sent_today.is_due(at("2026-11-09", "09:30")));
        let sent_yesterday = subscription(Some("2026-11-08"));
        assert!(sent_yesterday.is_due(at("2026-11-09", "08:00")));
    }

    #[test]
    fn skips_the_day_when_too_late_to_catch_up() {
        let fresh = subscription(None);
        assert!(fresh.is_due(at("2026-11-09", "13:59")));
        assert!(!fresh.is_due(at("2026-11-09", "14:00")));
    }

    #[test]
    fn first_post_is_tomorrow_if_the_time_has_passed() {
        let schedule = temp_schedule("first");
        let later = schedule
            .subscribe(1, time("08:00"), at("2026-11-09", "10:00"))
            .unwrap();
        assert_eq!(later.last_sent, Some("2026-11-09".parse().unwrap()));
        let earlier = schedule
            .subscribe(2, time("08:00"), at("2026-11-09", "06:00"))
            .unwrap();
        assert_eq!(earlier.last_sent, None);
    }

    #[test]
    fn subscribing_again_replaces_the_time() {
        let schedule = temp_schedule("replace");
        let now = at("2026-11-09", "06:00");
        schedule.subscribe(1, time("08:00"), now).unwrap();
        schedule.subscribe(1, time("09:15"), now).unwrap();
        assert_eq!(schedule.lock().len(), 1);
        assert_eq!(schedule.get(1).unwrap().time, time("09:15"));
    }

    #[test]
    fn takes_due_channels_once_and_persists() {
        let schedule = temp_schedule("due");
        let morning = at("2026-11-09", "06:00");
        schedule.subscribe(1, time("08:00"), morning).unwrap();
        schedule.subscribe(2, time("20:00"), morning).unwrap();

        let eight = at("2026-11-09", "08:00");
        assert_eq!(schedule.take_due(eight), [1]);
        assert!(schedule.take_due(eight).is_empty());

        let reopened = Schedule::open(schedule.path.clone()).unwrap();
        assert_eq!(reopened.get(1).unwrap().last_sent, Some(eight.date()));
    }

    #[test]
    fn unsubscribing_reports_whether_anything_was_there() {
        let schedule = temp_schedule("unsubscribe");
        schedule
            .subscribe(1, time("08:00"), at("2026-11-09", "06:00"))
            .unwrap();
        assert!(schedule.unsubscribe(1).unwrap());
        assert!(!schedule.unsubscribe(1).unwrap());
    }

    #[test]
    fn reads_genres_from_model_replies() {
        assert_eq!(parse_genre("Jazz"), Some("jazz"));
        assert_eq!(parse_genre("I'd say k-pop."), Some("k-pop"));
        assert_eq!(parse_genre("**Acoustic**"), Some("acoustic"));
        assert_eq!(parse_genre("something calm"), None);
    }

    #[test]
    fn formats_the_post() {
        let quote = Quote {
            text: "Begin anywhere.".into(),
            author: "John Cage".into(),
        };
        let track = Track {
            name: "So What".into(),
            artists: vec!["Miles Davis".into()],
            album: "Kind of Blue".into(),
            url: "https://open.spotify.com/track/abc".into(),
        };
        let today = "2026-11-09".parse().unwrap();
        assert_eq!(
            format_post(today, Some(&quote), Some(&("jazz", track))),
            "☀️ **Good morning!** Here's your quote for Monday, 9 November.\n\
             > Begin anywhere.\n\
             > — John Cage\n\
             \n\
             🎵 **Today's song** (jazz): So What by Miles Davis\n\
             https://open.spotify.com/track/abc\n\
             -# Quote from <https://zenquotes.io>"
        );
    }

    #[test]
    fn says_so_when_the_quote_is_missing() {
        let today = "2026-11-09".parse().unwrap();
        assert_eq!(
            format_post(today, None, None),
            "☀️ **Good morning!** I couldn't fetch today's quote."
        );
    }

    #[test]
    #[ignore = "requires Apple Intelligence"]
    fn model_picks_a_genre_from_the_list() {
        let engine = Engine::new().unwrap();
        let quotes = [
            ("The best way out is always through.", "Robert Frost"),
            ("Joy is the simplest form of gratitude.", "Karl Barth"),
            (
                "In the middle of difficulty lies opportunity.",
                "Albert Einstein",
            ),
        ];
        for (text, author) in quotes {
            let quote = Quote {
                text: text.into(),
                author: author.into(),
            };
            let reply = engine.ask_once(&genre_prompt(&quote)).unwrap();
            assert!(parse_genre(&reply).is_some(), "{text:?} got {reply:?}");
        }
    }

    #[tokio::test]
    #[ignore = "requires the network"]
    async fn fetches_the_real_quote_of_the_day() {
        let quote = fetch_quote(&reqwest::Client::new()).await.unwrap().unwrap();
        assert!(
            !quote.text.is_empty() && !quote.author.is_empty(),
            "{quote:?}"
        );
    }
}
