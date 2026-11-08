//! Song recommendations from Spotify.

use super::{Args, BeanTool, Param, ToolContext, ToolError, ToolResult};

pub struct RecommendSong;

impl BeanTool for RecommendSong {
    fn name(&self) -> &'static str {
        "recommendSong"
    }

    fn description(&self) -> &'static str {
        "Picks a real song on Spotify. Pass a genre like 'jazz', 'k-pop', or 'acoustic' for a \
         random song in it, or an artist or song name to look one up. Use it whenever someone \
         asks for music or a song recommendation."
    }

    fn params(&self) -> &'static [Param] {
        const PARAMS: &[Param] = &[Param::text(
            "query",
            "A genre, mood, artist, or song, e.g. 'jazz' or 'Taylor Swift'",
        )];
        PARAMS
    }

    fn available(&self, ctx: &ToolContext) -> bool {
        ctx.spotify().is_some()
    }

    fn run(&self, args: &Args, ctx: &ToolContext) -> ToolResult {
        let spotify = ctx
            .spotify()
            .ok_or_else(|| ToolError::new("Spotify isn't set up"))?;
        let query = args.text("query").unwrap_or("pop");
        ctx.record(format!("Found a {query} song on Spotify"));

        let track = ctx
            .block_on(spotify.random_track(query))?
            .map_err(|e| ToolError::new(format!("Spotify search failed: {e}")))?
            .ok_or_else(|| ToolError::new(format!("no songs found for {query:?}")))?;
        ctx.attach(&track.url);
        Ok(format!(
            "{} (from the album {}). A Spotify link is posted under your reply automatically, \
             so don't write a link yourself.",
            track.title(),
            track.album
        ))
    }
}
