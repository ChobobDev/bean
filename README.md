# Bean

A Discord chat bot powered by Apple's on-device [Foundation Models](https://developer.apple.com/documentation/foundationmodels), written in Rust. Bean runs entirely on your Mac: messages never leave it for a cloud AI service.

## Features

- **Conversation memory.** Each channel (and DM) has its own conversation. Bean remembers what was said and who said it, and the memory survives restarts.
- **Automatic summarizing.** The on-device model has a small context window (~4k tokens). When a conversation gets too long, Bean summarizes the older part and carries on from the summary.
- **Streaming replies.** Bean posts its reply as soon as it starts writing and fills it in as the model generates. Long replies continue across messages without breaking code blocks.
- **Slash commands.** `/reset` makes Bean forget the current channel. `/memory` shows how full that channel's memory is.
- **Safe output.** Replies can never ping `@everyone`, roles, or users.

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

Conversations are stored as plain JSON in `$BEAN_DATA_DIR/conversations/<channel id>.json`. Delete a file (or use `/reset`) to forget that channel.

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
