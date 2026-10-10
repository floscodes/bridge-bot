# bridge-bot

A bridge bot written in Rust that forwards text messages between **Telegram**, **Reticulum LXMF**, and **Reticulum Relay Chat (RRC)**. Connect groups, forum topics, LXMF peers, and RRC rooms through explicitly configured routes.

```text
Telegram ↔ bridge-bot ↔ LXMF / RRC
```

### AI Disclosure

AI tools were used to assist with the development of this project.

### Features

- One-way and bidirectional routes, including multiple destinations.
- Explicit mapping of Telegram topics to RRC rooms.
- Embedded Reticulum stack with separate interfaces and identities for each service.
- Multiple instances of the same service, such as different Telegram bots or RRC hubs.
- Bounded in-memory delivery retries and diagnostic tools.

### Future plans

**More services might be added in the future.** The adapter structure supports adding further services; Telegram, LXMF, and RRC are currently supported.

Forwarded messages start with:

```text
Alice wrote via Telegram:
Hello!
```

## Setup

You need a current stable Rust toolchain supporting Edition 2024 and access to the networks you want to use.

1. Create a Telegram bot through @BotFather and set its token in `bridge-bot.toml` as `token` under `[services.telegram]`. Add the bot to the desired group or channel. For groups, disable Privacy Mode through BotFather and re-add the bot if necessary. For channels, the bot needs administrator rights and permission to publish messages. Users must initiate private chats themselves.
2. For LXMF and RRC, configure the desired Reticulum interfaces directly in each service. Both services run their own Rust Reticulum stack inside the bot process; no separate installation or LXMF daemon is required.
3. Copy the example configuration and insert your Telegram chat IDs, LXMF peer destination hashes, and RRC hub addresses if needed. The bot's own LXMF delivery destination is derived from `identity_path` and logged at startup. The example addresses are placeholders.

```sh
cp bridge-bot.toml.example bridge-bot.toml
cargo run -- --check
cargo run --release
```

The default configuration file is `bridge-bot.toml`, in TOML format. Use `--config file.toml` to select another path. `--check` only validates the configuration; it does not connect to services or verify the token with Telegram. The local configuration is excluded by `.gitignore`. The Telegram token is read directly from the `token` field. The optional `[retry]` table appears at the end of the example; omitting it uses the defaults shown there.

Run `cargo run --bin bridge-check` to check Telegram authentication, access to configured groups, forum status, and bot membership. It does not send messages or fetch Telegram updates, and it does not print the token. `Receives all group messages: false` means Privacy Mode is enabled. As an ordinary group member, the bot will then miss some regular messages. Set `/setprivacy` to `Disable` through BotFather and re-add the bot, or grant it administrator rights. Routing does not require the current topic name; the configured `message_thread_id` must refer to an existing topic.

## Routes

Run `cargo run --bin bridge-check -- --rrc` to also check connections to configured RRC hubs and room joins. This does not send chat messages. The optional `--rrc-test-message "Test message"` argument explicitly sends a message to the single configured RRC room and waits for its hub echo. The bridge bot must be running at the same time to test forwarding. The checker does not fetch Telegram updates, so it does not compete with the bot's polling.

To test Telegram → RRC, wait for a marker with `--rrc --expect-rrc-text TESTMARKER --wait-secs 300` and post that marker in the configured Telegram topic. With Privacy Mode enabled, use a command addressed directly to the bot, such as `/bridge_test@BOT_USERNAME TESTMARKER`. The checker reports success only after the second RRC participant actually receives the forwarded message.

All bot log messages appear directly in the terminal on standard output, including debug and trace messages. `RUST_LOG` can additionally enable logs from dependencies; the bot's own messages always remain visible.

Services and routes are optional. With no configured services, the bot logs `no services registered` and exits successfully. Routes to unconfigured services remain inactive without producing errors. RRC does not start without an active room route. Errors in explicitly configured services are still reported.

```toml
[[bridges]]
from = { service = "telegram", chat = "-1001234567890" }
to = { service = "lxmf", chat = "0123456789abcdef0123456789abcdef" }
bidirectional = true
```

`bidirectional = true` enables both directions; `false` or an omitted field enables only `from` → `to`. Multiple entries allow multiple chats and destinations. Identical destinations are served only once per incoming message. Only configured source chats or LXMF senders are forwarded.

Service names are configurable: multiple Telegram bots, LXMF identities, or RRC hubs can have their own entries under `[services.NAME]`. The adapter is inferred for the standard names `telegram`, `lxmf`, and `rrc`, where `type` is optional. Custom names such as `[services.telegram_work]` require an explicit type, for example `type = "telegram"`. An explicit type takes precedence over the name. LXMF names are mapped by delivery destination in `[services.NAME.usernames]`; otherwise, the bot uses the name from the verified announce or the address. Telegram uses the username, display name, or channel name.

## Topics and rooms

Rooms are mapped explicitly in `[[bridges]]`. For Telegram, `chat` is always the required group chat ID; `message_thread_id` is the additional numeric ID of a topic within that group. For RRC, the room is specified exclusively in the `room` field. Names do not need to match:

```toml
[[bridges]]
from = { service = "telegram", chat = "-1001234567890", message_thread_id = 42 }
to = { service = "rrc", room = "#lobby" }
bidirectional = true

[[bridges]]
from = { service = "telegram", chat = "-1001234567890", message_thread_id = 57 }
to = { service = "rrc", room = "#news" }
bidirectional = true
```

Messages from topic 42 go only to `#lobby`, and messages from topic 57 go to `#news`; the reverse direction sends to the corresponding configured Telegram topic. To explicitly target several destinations, add multiple routes with the same source. Identical destinations are deduplicated per message. Other rooms are not selected automatically.

Omitting `message_thread_id` targets an ordinary Telegram group without forum topics. If the destination group has topics, `message_thread_id` is required, including for `General` (`message_thread_id = 1`). If it is missing, the bot logs an error and sends nothing. Conversely, specifying a topic ID for a group without topics is an error. Messages from a Telegram topic or RRC room without a matching source route are logged as errors and are not forwarded. A Telegram entry without `message_thread_id` is not a wildcard for all topics.

Topic IDs remain unchanged when topics are renamed. The bot uses incoming [Telegram `message_thread_id` values](https://core.telegram.org/bots/api#message) directly and does not need to learn names. If the configured destination is closed or unavailable, forwarding ends with an error without falling back to another room. All message parts and delivery retries stay within the configured destination.

## Separate Reticulum interfaces per service

`[[services.rrc.interfaces]]` and `[[services.lxmf.interfaces]]` are independent lists. Each service can also have multiple interfaces:

```toml
[services.lxmf]
identity_path = ".bridge-bot/lxmf-identity"
display_name = "bridge-bot"

[[services.lxmf.interfaces]]
type = "tcp_client"
host = "lxmf-peer.example.org"
port = 4242

[services.rrc]
hub = "00000000000000000000000000000003"
identity_path = ".bridge-bot/rrc-identity"
nickname = "bridge-bot"

[[services.rrc.interfaces]]
type = "tcp_client"
host = "rrc-peer.example.org"
port = 4242
```

Supported types are `tcp_client` with `host`/`port`, `tcp_server` with `listen`/`port`, and `auto` for IPv6 peer discovery on the local network. For `auto`, optional fields are `group_id` (default: `reticulum`), `discovery_port` (29716), `data_port` (42671), and `devices` (a list of network interfaces, unrestricted by default). An AutoInterface alone does not connect to a remote network; a reachable peer is required. TCP ports must be between 1 and 65535.

If `interfaces` is omitted, each service uses an AutoInterface. `interfaces = []` explicitly disables network interfaces. Instances do not use a shared instance and do not share interfaces or routing tables. Configurations using the same local listening port can conflict; in particular, configure AutoInterfaces with suitable separate ports and peer settings when needed.

Network state is stored per service under `.bridge-bot/reticulum/managed-<hex-encoded service name>`. The optional `reticulum_config` sets the parent state directory; interfaces always come from the TOML file. Existing external Reticulum configuration files are neither overwritten nor imported automatically. If you previously configured interfaces in such a file, move them into the corresponding TOML list.

For LXMF, `identity_path` replaces the former `rpc_endpoint` and `source` fields. An existing raw 64-byte RNS identity can be reused to retain the same address. If no identity exists, the bot creates one. Preserve the identity file. Incoming messages are checked for destination address, sender identity, and signature before forwarding; unknown senders are resolved through a path request. If the signature cannot be verified, the bot logs a warning and discards the message.

`identity_path` is optional: it defaults to `.bridge-bot/rrc-identity` for `services.rrc` and `.bridge-bot/lxmf-identity` for `services.lxmf`. An existing file is loaded; otherwise, a new identity is saved there. Invalid or unreadable existing files cause an error and are not overwritten. Custom service names receive separate default files under `.bridge-bot/identity-<hex-encoded service name>`; an explicit path always takes precedence.

`reticulum-node` remains an optional standalone diagnostic tool. Normal bot operation does not require this process. `bridge-check --rrc` also runs its own embedded stack with the configured RRC interfaces for the second test participant; an additional TCP server interface should not use the same listening port as the bot.

## RRC rooms

RRC is a separate live chat service over identified Reticulum links, using CBOR messages sent to an `rrc.hub`. It does not use an LXMF delivery destination. Configure one service per hub; the `room` field identifies a room. Each service stores a persistent RNS identity.

Leading `#` characters are removed from configured room names: `room = "#general"` and `room = "general"` both use the room `general`.

```toml
[services.rrc]
hub = "00000000000000000000000000000003" # replace with the actual RRC hub destination
identity_path = ".bridge-bot/rrc-identity"
nickname = "bridge-bot"
reticulum_config = ".bridge-bot/reticulum"

[[bridges]]
from = { service = "telegram", chat = "-1001234567890" }
to = { service = "rrc", room = "#lobby" }
bidirectional = true
```

Room names must be lowercase. All rooms referenced by routes are joined after `WELCOME`; the adapter becomes ready only after `JOINED`. Messages (`MSG`) and actions (`ACTION`) are forwarded, while the bot's own echoes and hub notices are not. Nicknames are display hints supplied by the hub, not verified identities. Long text is split into multiple messages to fit the link packet size. Delivery is acknowledged by the hub echoing the bot's own message with a matching ID, room, and text. This confirms processing at the hub, not receipt by every room participant. The hub must therefore forward messages to their sender as well, as the reference hub `rrcd` does.

RRC and LXMF run entirely inside the bot through the Rust library [rsReticulum](https://github.com/ratspeak/rsReticulum), pinned to a specific Git revision. Each service has its own stack with the interfaces configured in TOML.

Preserve the identity file across restarts; existing raw 64-byte RNS identities can be reused. Use separate identity files for instances running concurrently. During normal shutdown, the bot leaves its rooms and shuts down the network instance cleanly.

If a link drops, the adapter attempts to reconnect and rejoin rooms with increasing delays. If the hub refuses a join or removes the bot from a room, the bot exits with an error. Hub rate limits trigger delivery retries; other explicitly reported send rejections end the affected forwarding operation. RRC messages sent to a room during a disconnection cannot be retrieved afterward. Room passwords, historical messages, and Resource extensions are not yet supported. Protocol: [RRC specification](https://rrc.kc1awv.net/3-RRC-wire-encoding.html).

## Delivery retries

The bot uses only bounded in-memory queues. After successful forwarding, the job is discarded; there is no message database or persistent outbox. Each destination chat has its own FIFO worker: a failing destination does not block other destination chats, and later messages do not overtake ongoing retries for the same destination.

```toml
[retry]
max_attempts = 6          # initial attempt plus at most five retries
initial_delay_secs = 2
max_delay_secs = 60
max_age_secs = 600       # measured from entry into the queue
attempt_timeout_secs = 90
queue_capacity = 256     # waiting jobs per destination chat
```

All fields are optional and use these defaults. Delays grow exponentially (roughly 2, 4, 8, 16, and 32 seconds), with up to 20% additional random jitter, capped by `max_delay_secs`. Telegram's `retry_after` is honored even if it exceeds this cap. A clearly permanent error, such as missing Telegram permissions, stops retries immediately. Retries and final failures are logged.

Telegram delivery is considered accepted after confirmation from the Bot API. LXMF sends signed messages as Reticulum Resources over a direct encrypted link and waits for the Resource receipt acknowledgment. This confirms transfer to the recipient, not display or reading in their application. If the recipient requires a delivery stamp, it is generated; the work is aborted on timeout. The message timestamp, signature, and ID remain unchanged across retries. An aborted attempt causes its link to close. RRC waits up to 30 seconds for the hub echo and preserves IDs of unconfirmed parts across retries. Already confirmed Telegram and RRC parts are skipped.

At most 4096 forwarding jobs with a combined 16 MiB of outgoing text are allowed; prepared packet data uses additional memory. Full queues, exhausted limits, timeouts, and shutdown cause affected jobs to be discarded and are logged. If an acknowledgment is lost, resending can produce duplicates even when the first delivery succeeded; there is no exactly-once guarantee. No retry jobs survive a restart.

## Current scope and limitations

- Telegram: private chats, groups, supergroups, and channels; text and media captions. Forum topics in supergroups are explicitly routed by thread ID. Long text is split according to the UTF-16 limit. Media files, edits, deletions, reply relationships, and channel comment threads are not yet forwarded.
- LXMF: direct delivery and receipt of direct link packets, Resources, and opportunistic destination packets. Messages are limited to 1 MiB. Sending through propagation nodes, tickets, media, and LXMF group relay protocols are not yet implemented. RRC rooms are supported through a separate adapter.
- The bot's own Telegram messages, its own LXMF senders, and duplicate incoming IDs are filtered during runtime. Caches are bounded and do not persist. Other relay bots can cause loops; multiple bridges should not form a cyclic relay chain.
- Telegram polling reconnects after connection errors; TCP client interfaces are reconnected by the embedded stack. Delivery failures are retried in memory according to `[retry]`; there is no persistent outbox.
- LXMF receives only while the bot is running; messages are not fetched after a restart. Telegram uses long polling: no other poller or webhook may be active for the same bot.

## Development

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings

# Rust RRC integration test over local TCP interfaces:
cargo test native_rrc
```

`src/config.rs` validates services and routes. `src/services/mod.rs` defines the common incoming message format and the `Adapter` trait. New services implement `receive` and `send` and extend the service configuration and adapter creation. The routing code does not require service-specific changes.

Libraries: [teloxide 0.17](https://docs.rs/teloxide/0.17.0/teloxide/), [lxmf-wire 0.13](https://crates.io/crates/lxmf-wire) for the LXMF message format, and [rsReticulum](https://github.com/ratspeak/rsReticulum) for the network stack. rsReticulum uses AGPL-3.0-or-later; LXMF-rs uses EPL-2.0 or GPL-2.0-or-later. Observe their license terms when distributing the software.
