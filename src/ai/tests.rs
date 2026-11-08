//! These tests talk to the real on-device model, so they need a Mac with
//! Apple Intelligence enabled. Run them with `cargo test -- --ignored`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fm_rs::{ContextLimit, SystemLanguageModel};

use super::{ConversationStore, Engine, Outcome};
use crate::reminders::{Reminders, unix_now};
use crate::tools::{DiscordChannel, Toolkit};

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bean-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn engine() -> Engine {
    let engine = Engine::new().expect("model");
    engine
        .ensure_available()
        .expect("Apple Intelligence available");
    engine
}

/// Tools call `Handle::block_on` from a dispatch thread, so the runtime
/// must drive its own timers and IO: a multi-threaded one.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| tokio::runtime::Runtime::new().unwrap())
}

fn open_store(engine: Engine, dir: &Path) -> ConversationStore {
    let handle = runtime().handle().clone();
    let reminders = Reminders::open(dir.join("reminders.json"), handle.clone()).unwrap();
    ConversationStore::open(engine, Toolkit::new(handle, reminders), dir).unwrap()
}

fn ask(store: &ConversationStore, channel: u64, prompt: &str) -> (String, Outcome) {
    let conversation = store.conversation(channel, None);
    let mut conversation = conversation.try_lock().expect("uncontended");
    conversation
        .respond(&store.engine(), Some(42), prompt, |_| {})
        .expect("response")
}

#[test]
#[ignore = "requires Apple Intelligence"]
fn remembers_across_turns_and_restarts() {
    let dir = temp_dir("restart");

    let store = open_store(engine(), &dir);
    ask(
        &store,
        1,
        "Remember this: my cat is named Biscuit. Just reply OK.",
    );
    drop(store);

    let store = open_store(engine(), &dir);
    let (reply, _) = ask(
        &store,
        1,
        "What is my cat's name? Answer with just the name.",
    );
    assert!(
        reply.to_lowercase().contains("biscuit"),
        "reply was {reply:?}"
    );

    let (reply, _) = ask(
        &store,
        2,
        "What is my cat's name? If you don't know, say UNKNOWN.",
    );
    assert!(
        !reply.to_lowercase().contains("biscuit"),
        "channels leaked: {reply:?}"
    );

    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires Apple Intelligence"]
fn streams_reply_in_chunks() {
    let dir = temp_dir("stream");
    let store = open_store(engine(), &dir);
    let conversation = store.conversation(1, None);
    let mut conversation = conversation.try_lock().unwrap();

    let mut chunks = Vec::new();
    let (text, _) = conversation
        .respond(
            &store.engine(),
            None,
            "Write three sentences about the sea.",
            |chunk| {
                chunks.push(chunk.to_owned());
            },
        )
        .unwrap();

    assert!(
        chunks.len() > 1,
        "expected incremental chunks, got {chunks:?}"
    );
    assert_eq!(chunks.concat(), text);

    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires Apple Intelligence"]
fn calls_tools_and_keeps_them_after_restart() {
    let dir = temp_dir("tools");

    let store = open_store(engine(), &dir);
    let (reply, outcome) = ask(&store, 1, "What is today's date?");
    assert_eq!(
        outcome.tools_used,
        ["Checked the time"],
        "reply was {reply:?}"
    );
    drop(store);

    // A restored conversation must still be able to call tools.
    let store = open_store(engine(), &dir);
    let (reply, outcome) = ask(&store, 1, "What's the weather like in Seoul right now?");
    assert_eq!(
        outcome.tools_used,
        ["Checked the weather in Seoul"],
        "reply was {reply:?}"
    );
    assert!(reply.contains('°'), "reply was {reply:?}");

    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires Apple Intelligence"]
fn answering_without_tools_keeps_memory_and_tools() {
    let dir = temp_dir("no-tools");
    let store = open_store(engine(), &dir);
    ask(
        &store,
        1,
        "Remember this: my cat is named Biscuit. Just reply OK.",
    );

    let reply = {
        let conversation = store.conversation(1, None);
        let mut conversation = conversation.try_lock().unwrap();
        let mut text = String::new();
        conversation
            .answer_without_tools(
                &store.engine(),
                "What is my cat's name?",
                &mut text,
                &mut |_| {},
            )
            .unwrap();
        text
    };
    assert!(
        reply.to_lowercase().contains("biscuit"),
        "reply was {reply:?}"
    );

    let (reply, outcome) = ask(&store, 1, "What is today's date?");
    assert_eq!(
        outcome.tools_used,
        ["Checked the time"],
        "reply was {reply:?}"
    );

    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires Apple Intelligence"]
fn sets_reminders_for_the_speaker() {
    let dir = temp_dir("reminder");
    let handle = runtime().handle().clone();
    let reminders = Reminders::open(dir.join("reminders.json"), handle.clone()).unwrap();
    let store =
        ConversationStore::open(engine(), Toolkit::new(handle, Arc::clone(&reminders)), &dir)
            .unwrap();
    // The reminder tool doesn't touch Discord until delivery, so a
    // client that never connects is enough.
    let discord = DiscordChannel {
        http: Arc::new(serenity::all::Http::new("")),
        channel_id: serenity::all::ChannelId::new(5),
        guild_id: None,
    };
    let conversation = store.conversation(5, Some(discord));
    let mut conversation = conversation.try_lock().unwrap();
    let (reply, outcome) = conversation
        .respond(
            &store.engine(),
            Some(42),
            "Remind me in 2 hours to stretch.",
            |_| {},
        )
        .unwrap();

    let pending = reminders.pending_for(42);
    assert_eq!(
        pending.len(),
        1,
        "reply was {reply:?}, tools {:?}",
        outcome.tools_used
    );
    assert!(
        pending[0].text.to_lowercase().contains("stretch"),
        "{pending:?}"
    );
    let delay = pending[0].due_at - unix_now();
    assert!((110 * 60..=120 * 60).contains(&delay), "due in {delay}s");
    assert_eq!(pending[0].channel_id, 5);

    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires Apple Intelligence"]
fn compacts_when_context_fills_up() {
    let dir = temp_dir("compact");
    let model = SystemLanguageModel::new().unwrap();
    let tiny = ContextLimit::new(500).with_reserved_response_tokens(100);
    let store = open_store(Engine::with_limit(model, tiny), &dir);

    ask(
        &store,
        1,
        "My name is Juniper and I live in Lisbon. Reply OK.",
    );
    let mut compacted = false;
    for topic in ["volcanoes", "octopuses", "the Roman empire", "jazz"] {
        let (_, outcome) = ask(
            &store,
            1,
            &format!("Write a detailed paragraph about {topic}."),
        );
        compacted |= outcome.compacted;
    }
    assert!(compacted, "expected the tiny context to force a compaction");

    let status = {
        let conversation = store.conversation(1, None);
        let mut conversation = conversation.try_lock().unwrap();
        conversation.status(&store.engine()).unwrap()
    };
    assert!(status.compactions > 0);

    fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "requires Apple Intelligence"]
fn reset_forgets_everything() {
    let dir = temp_dir("reset");
    let store = open_store(engine(), &dir);
    ask(&store, 1, "Remember the code word: PELICAN. Reply OK.");
    assert!(dir.join("1.json").exists());

    store
        .conversation(1, None)
        .try_lock()
        .unwrap()
        .reset()
        .unwrap();
    assert!(!dir.join("1.json").exists());

    let (reply, _) = ask(
        &store,
        1,
        "What was the code word? If you don't know, say UNKNOWN.",
    );
    assert!(
        !reply.to_uppercase().contains("PELICAN"),
        "reply was {reply:?}"
    );

    fs::remove_dir_all(dir).unwrap();
}
