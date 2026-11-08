# Bean

A Discord chat bot powered by Apple's on-device [Foundation Models](https://developer.apple.com/documentation/foundationmodels), written in Rust. Bean runs entirely on your Mac: messages never leave it for a cloud AI service.

## Features

- **Conversation memory.** Each channel (and DM) has its own conversation. Bean remembers what was said and who said it, and the memory survives restarts.
- **Automatic summarizing.** The on-device model has a small context window (~4k tokens). When a conversation gets too long, Bean summarizes the older part and carries on from the summary.
- **Streaming replies.** Bean posts its reply as soon as it starts writing and fills it in as the model generates. Long replies continue across messages without breaking code blocks.
- **Tools.** Bean can take actions while answering (see below). Each reply notes which tools it used.
- **Slash commands.** `/reset` makes Bean forget the current channel. `/memory` shows how full that channel's memory is.
- **Safe output.** Replies can never ping `@everyone`, roles, or users.

## Tools

The model decides when to call these on its own:

| Tool | What it does |
|---|---|
| `getCurrentTime` | The current date, time, and time zone (the model has no clock) |
| `readRecentMessages` | Reads up to 30 recent messages in the current channel, including ones not sent to Bean |
| `getServerInfo` | The server's name, member and online counts, creation date, roles, and channels |
| `setReminder` | Pings the person who asked, in the same channel, after a delay of up to 7 days |
| `listReminders` | Lists the asker's pending reminders |
| `getWeather` | Current weather for a city, from [Open-Meteo](https://open-meteo.com) (no API key) |

Tools only see the channel and server they're used in. Reminders are saved to disk and survive restarts, and each user can have up to 10 pending. There's deliberately no general "fetch a URL" tool: anyone in your server could use it to make your Mac request local network addresses.

Bean answers when it's @mentioned (as a user or via its role) in a server, and to every message in DMs.

## Requirements

- An Apple Silicon Mac running macOS 26 or later, with Apple Intelligence turned on
- Xcode (the Command Line Tools alone can't build the Foundation Models bindings). `.cargo/config.toml` points the build at `/Applications/Xcode.app`.
- A Discord bot with **Message Content Intent** enabled. Invite it with the `bot` and `applications.commands` scopes and permissions `68608` (View Channels, Send Messages, Read Message History).

## Running

```sh
DISCORD_TOKEN=your_token cargo run --release
```

| Variable | Default | Purpose |
|---|---|---|
| `DISCORD_TOKEN` | required | Bot token from the Discord Developer Portal |
| `BEAN_DATA_DIR` | `data` | Where conversations are saved |
| `RUST_LOG` | `bean=info,warn` | Log filter, e.g. `bean=debug` to log every message |

Conversations are stored as plain JSON in `$BEAN_DATA_DIR/conversations/<channel id>.json`. Delete a file (or use `/reset`) to forget that channel. Pending reminders are in `$BEAN_DATA_DIR/reminders.json`.

## Testing

```sh
cargo test                            # unit tests
cargo test -- --ignored --test-threads=1   # tests against the real on-device model
```

The ignored tests need Apple Intelligence available on the machine running them.

## Layout

| File | Responsibility |
|---|---|
| `src/main.rs` | Startup and configuration |
| `src/handler.rs` | Discord events: messages and slash commands |
| `src/conversation.rs` | Per-channel sessions, persistence, and summarizing |
| `src/reply.rs` | Streaming a reply into Discord messages |
| `src/pages.rs` | Splitting long text into message-sized pages |
| `src/prompt.rs` | Mention handling and user-facing error messages |
| `src/transcript.rs` | Rendering saved conversations for restoring them with tools |
| `src/tools/` | The tools the model can call |
| `src/reminders.rs` | Saving, scheduling, and delivering reminders |

## Notes on the on-device model

`fm-rs` exposes all tools to the model through one generic `invoke_tool(toolName, arguments)` function. The ~3B on-device model handles that imperfectly, and Bean works around it:

- It often mislabels arguments, e.g. using the tool's own name as the argument name. Schemas don't mark arguments as required, and tools recover the intended value when it's unambiguous.
- It sometimes calls tools that don't exist, which would fail the whole reply. Bean retries such turns without tools.
- `Session::from_transcript` drops tools, and `set_transcript` needs the macOS 27 SDK, so saved conversations are restored as text in a new session's instructions.

Building with the macOS 27 SDK (Xcode 27) would allow cleaner versions of the last two.
