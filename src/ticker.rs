//! Running scheduled work on time, even across the Mac sleeping.
//!
//! Tokio's timers run on a monotonic clock that stops while macOS is
//! asleep, so "sleep until 8:00" set at night fires late by however long
//! the lid was closed. Scheduled work instead checks the wall clock every
//! [`CHECK_EVERY`]; after the Mac wakes, the next check catches up at once.

use std::future::Future;
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::time::MissedTickBehavior;

pub const CHECK_EVERY: Duration = Duration::from_secs(30);

/// Calls `check` every [`CHECK_EVERY`], forever. Each check finishes before
/// the next starts, so work is never done twice in parallel.
pub fn spawn<F, Fut>(runtime: &Handle, mut check: F)
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    runtime.spawn(async move {
        let mut interval = tokio::time::interval(CHECK_EVERY);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            check().await;
        }
    });
}
