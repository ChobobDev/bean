//! Finding songs on Spotify.
//!
//! Uses the client credentials flow, so no user has to sign in. Spotify
//! closed its recommendations endpoint to new apps in 2024 and limits what
//! Development Mode apps can read, but search still works. A random song
//! in a genre is a `genre:` search at a random offset.

use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::sync::Mutex;

/// Spotify's maximum page size for search.
const PAGE: usize = 10;
/// Spotify's maximum search offset.
const MAX_OFFSET: usize = 1000;
/// Refresh tokens this long before Spotify says they expire.
const TOKEN_MARGIN: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct Credentials {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub name: String,
    pub artists: Vec<String>,
    pub album: String,
    /// open.spotify.com link; Discord shows it as a player.
    pub url: String,
}

impl Track {
    /// "Title by Artist A, Artist B"
    pub fn title(&self) -> String {
        format!("{} by {}", self.name, self.artists.join(", "))
    }
}

pub struct Spotify {
    credentials: Credentials,
    web: reqwest::Client,
    token: Mutex<Option<(String, Instant)>>,
}

impl Spotify {
    pub fn new(credentials: Credentials, web: reqwest::Client) -> Self {
        Self {
            credentials,
            web,
            token: Mutex::new(None),
        }
    }

    /// A random song in `genre`. If Spotify knows no such genre (it's a
    /// mood, or an artist's name), falls back to a plain search for it.
    pub async fn random_track(&self, genre: &str) -> reqwest::Result<Option<Track>> {
        let query = format!("genre:\"{}\"", genre.replace('"', ""));
        let first = self.search(&query, 0).await?;
        if first.total == 0 {
            return Ok(pick(self.search(genre, 0).await?.tracks()));
        }
        let offset = random_offset(first.total);
        let page = if offset == 0 {
            first
        } else {
            self.search(&query, offset).await?
        };
        Ok(pick(page.tracks()))
    }

    async fn search(&self, query: &str, offset: usize) -> reqwest::Result<TrackPage> {
        let token = self.token().await?;
        let response: SearchResponse = self
            .web
            .get("https://api.spotify.com/v1/search")
            .bearer_auth(token)
            .query(&[
                ("q", query),
                ("type", "track"),
                ("limit", &PAGE.to_string()),
                ("offset", &offset.to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(response.tracks)
    }

    /// A valid access token, fetching a new one when needed.
    async fn token(&self) -> reqwest::Result<String> {
        let mut cached = self.token.lock().await;
        if let Some((token, expires)) = cached.as_ref()
            && Instant::now() < *expires
        {
            return Ok(token.clone());
        }
        let response: TokenResponse = self
            .web
            .post("https://accounts.spotify.com/api/token")
            .basic_auth(
                &self.credentials.client_id,
                Some(&self.credentials.client_secret),
            )
            .form(&[("grant_type", "client_credentials")])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let lifetime = Duration::from_secs(response.expires_in).saturating_sub(TOKEN_MARGIN);
        *cached = Some((response.access_token.clone(), Instant::now() + lifetime));
        Ok(response.access_token)
    }
}

/// A random page start among `total` results, within Spotify's limits.
fn random_offset(total: usize) -> usize {
    let last = total.min(MAX_OFFSET).saturating_sub(PAGE);
    if last == 0 {
        0
    } else {
        fastrand::usize(0..=last)
    }
}

fn pick(tracks: Vec<Track>) -> Option<Track> {
    if tracks.is_empty() {
        None
    } else {
        let i = fastrand::usize(..tracks.len());
        tracks.into_iter().nth(i)
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

#[derive(Deserialize)]
struct SearchResponse {
    tracks: TrackPage,
}

#[derive(Deserialize)]
struct TrackPage {
    #[serde(default)]
    total: usize,
    #[serde(default)]
    items: Vec<Option<RawTrack>>,
}

impl TrackPage {
    fn tracks(self) -> Vec<Track> {
        self.items
            .into_iter()
            .flatten()
            .filter_map(RawTrack::into_track)
            .collect()
    }
}

#[derive(Deserialize)]
struct RawTrack {
    name: String,
    artists: Vec<Named>,
    album: Named,
    external_urls: ExternalUrls,
}

#[derive(Deserialize)]
struct Named {
    name: String,
}

#[derive(Deserialize)]
struct ExternalUrls {
    spotify: Option<String>,
}

impl RawTrack {
    fn into_track(self) -> Option<Track> {
        Some(Track {
            name: self.name,
            artists: self.artists.into_iter().map(|a| a.name).collect(),
            album: self.album.name,
            url: self.external_urls.spotify?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from the shape documented for `GET /v1/search`.
    const SEARCH: &str = r#"{
      "tracks": {
        "total": 2,
        "items": [
          {
            "name": "So What",
            "artists": [{ "name": "Miles Davis" }],
            "album": { "name": "Kind of Blue" },
            "external_urls": { "spotify": "https://open.spotify.com/track/abc" }
          },
          null,
          {
            "name": "No Link",
            "artists": [{ "name": "Someone" }],
            "album": { "name": "" },
            "external_urls": {}
          }
        ]
      }
    }"#;

    #[test]
    fn parses_search_results_and_skips_unlinkable_tracks() {
        let page = serde_json::from_str::<SearchResponse>(SEARCH)
            .unwrap()
            .tracks;
        assert_eq!(page.total, 2);
        let tracks = page.tracks();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].title(), "So What by Miles Davis");
        assert_eq!(tracks[0].url, "https://open.spotify.com/track/abc");
    }

    #[test]
    fn offsets_stay_within_spotify_limits() {
        assert_eq!(random_offset(0), 0);
        assert_eq!(random_offset(7), 0);
        for _ in 0..1000 {
            let offset = random_offset(50_000);
            assert!(offset <= MAX_OFFSET - PAGE, "{offset}");
        }
        for _ in 0..100 {
            assert!(random_offset(25) <= 15);
        }
    }

    #[test]
    fn picks_nothing_from_nothing() {
        assert_eq!(pick(Vec::new()), None);
    }

    /// Needs SPOTIFY_CLIENT_ID and SPOTIFY_CLIENT_SECRET.
    #[tokio::test]
    #[ignore = "requires Spotify credentials"]
    async fn finds_real_songs() {
        let credentials = Credentials {
            client_id: std::env::var("SPOTIFY_CLIENT_ID").expect("SPOTIFY_CLIENT_ID"),
            client_secret: std::env::var("SPOTIFY_CLIENT_SECRET").expect("SPOTIFY_CLIENT_SECRET"),
        };
        let spotify = Spotify::new(credentials, reqwest::Client::new());
        for query in ["jazz", "k-pop", "BTS", "rainy day"] {
            let track = spotify.random_track(query).await.unwrap();
            let track = track.unwrap_or_else(|| panic!("no song for {query}"));
            println!("{query}: {} {}", track.title(), track.url);
            assert!(track.url.starts_with("https://open.spotify.com/track/"));
        }
    }
}
