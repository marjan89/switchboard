# switchboard

File-based pub/sub for multi-Claude-session coordination. One log per
channel, one cursor per subscriber, JSONL on disk and on the wire. No
daemon, no sockets, no auth.

## What it's for

When multiple Claude Code sessions need to coordinate on a shared task —
a unified dashboard, a schema migration spread across two repos, an
integration where one session has the generator and another has the
renderer — switchboard gives them a channel to talk. The medium itself
turns out to be unusually load-bearing: append-only + tone-blind +
no-edit-button forces tighter writing than any chat tool.

Switchboard is the transport. Filtering ("only show messages addressed
to me"), rendering, and routing are the consumer's job (typically a
Monitor process feeding lines into a session as notifications).

## Install

```sh
git clone git@github.com:marjan89/switchboard.git /Users/Shared/projects/substrate-distro/switchboard
cd /Users/Shared/projects/substrate-distro/switchboard
cargo build --release
ln -sf $PWD/target/release/switchboard /opt/homebrew/bin/switchboard
ln -sf $PWD/target/release/switchboard-token-watcher /opt/homebrew/bin/switchboard-token-watcher
```

Verify:

```sh
switchboard --help
SWITCHBOARD_NAME=test switchboard send hello
```

## Workspace layout

The repo is a Cargo workspace. Switchboard itself is one crate; bundled
maintenance bots are siblings.

```
switchboard/
├── Cargo.toml                     workspace root
├── src/
│   ├── lib.rs                     library — paths, record, io, stream, cli, cmd
│   └── main.rs                    the `switchboard` binary
└── bots/
    └── token-watcher/             the `switchboard-token-watcher` binary
        ├── Cargo.toml
        └── src/main.rs            depends on switchboard via path = "../.."
```

Why bots are workspace crates, not subcommands of `switchboard`:

- Bots are participants. They use the public envelope and filesystem
  layout, exactly like a human-driven session would. Embedding one as a
  subcommand would invite reaching into switchboard's private types
  instead of going through the wire.
- Future LLM-driven bots can't be Rust-in-switchboard (they'll be Python
  with the Anthropic SDK, or whatever). Setting the precedent that bots
  are *separate processes* keeps the door open without architectural
  churn.

Maintenance bots ship in this repo. Third-party / LLM-driven bots live
elsewhere and just need the `switchboard` binary on PATH.

## Layout

On disk:

```
$SWITCHBOARD_DIR/                       default ~/.cache/switchboard/
├── <channel>/
│   ├── log                             JSONL append-only
│   ├── peers/<handle>                  presence file (mtime = last activity)
│   └── cursor.<handle>                 optional pull cursor
```

`SWITCHBOARD_DIR` env override is respected. Channels are bare directory
names; first `send` to a channel creates it.

## Wire format

JSONL. One record per line, both on disk and on stdout from `stream`/`log`.

```json
{"ts":"2026-05-06T22:55:01Z","ch":"default","kind":"message","from":"alice","to":["bob","ops"],"body":"..."}
{"ts":"2026-05-06T22:55:09Z","ch":"default","kind":"join","handle":"carol","cwd":"/Users/Shared/projects/foo"}
{"ts":"2026-05-06T22:56:14Z","ch":"default","kind":"leave","handle":"alice"}
{"ts":"2026-05-06T22:57:00Z","ch":"default","kind":"roster","members":[{"handle":"alice","last_seen":"..."}]}
{"ts":"2026-05-06T22:57:00Z","ch":"default","kind":"ready"}
{"ts":"2026-05-06T22:57:30Z","ch":"default","kind":"service_announcement","from":"bot-token-watcher","body":"⚠ alice (claude-opus-4-7) at 168.0k/200.0k (84%) — consider /compact","level":"warning"}
{"ts":"2026-05-06T22:58:00Z","ch":"default","kind":"rotated"}
```

Kinds:

- `message` — body addressed to the channel; `to` is an opaque string array
  (handles or groups), omitted/empty for broadcasts.
- `join` / `leave` — presence transitions. `join` is auto-emitted on a
  handle's first `send` to a channel and stamps the sender's `cwd`. The
  `cwd` is set by switchboard itself (not user-supplied), so subscribers
  can trust it for transcript-discovery without participant cooperation.
- `service_announcement` — the loud-voice channel. Carries `body` (text)
  and `level` (`warning` / `critical`). Conventionally emitted by bots
  when something warrants attention. Distinct kind so participants /
  Monitor can route or render differently from chatter.
- `roster` — synthesized by `stream` at startup, lists currently active
  peers (peers/<h> mtime within the staleness threshold). Not persisted.
- `ready` — synthesized boundary marker between any startup backlog and
  the live tail.
- `rotated` — synthesized when the log file's inode changes (rotation,
  external truncation). Subscribers should expect a re-read.

Handles are role-blind. If you want to convey role, encode it in the
handle (`operator-sinisa`, `ios-custodian`, `zealot-test`).

## Identity

`SWITCHBOARD_NAME` (env) or `--handle` (flag) — required for any command
that writes (`send`, `leave`, `mark-read`) and for `stream --from-cursor`.

`SWITCHBOARD_CHANNEL` (env) or `--channel` (flag) — defaults to `default`.

## Commands

```
switchboard send [body...]                  append a message; "-" for stdin, -f <file> for file
            --to <h>,<h>,...                comma-separated targets (omit for broadcast)
switchboard leave                           emit kind:leave; remove peers/<handle>
switchboard stream                          long-running JSONL firehose
            --all                           every channel under $SWITCHBOARD_DIR (current and future)
            --from-start                    replay full log before going live
            --from-cursor                   resume from cursor.<handle>; advance as records flow
switchboard log                             one-shot replay (no follow)
            --since <iso8601>               only records with ts >= this
            --kind <k>                      filter by kind
            --from <handle>                 filter by author
switchboard channels                        JSONL: {ch, peers_active, last_event_ts}
            --all                           include channels with no active peers
switchboard peers                           JSONL: {handle, last_seen, stale}
switchboard mark-read                       advance cursor.<handle> to current EOF
```

## Architecture notes

### The file IS the protocol

Two load-bearing properties:

1. **Append-only kills write-races** at the macOS atomic-write boundary.
   Bodies are capped at 4 KB so a single POSIX append stays atomic
   (PIPE_BUF guarantee); two senders writing the same instant produce two
   complete JSON lines, never a half-line.
2. **Plain JSONL is debuggable.** `cat`, `jq`, `grep` all work. The log is
   the system of record; recovery on any process death is "the file is
   still there."

### State, derived state, no state

The log is canonical. Everything else is a derivable cache:

- `peers/<handle>` mtimes — fast "who's active right now" probe;
  rebuildable by replaying join/leave events against a staleness
  threshold (5 min).
- `cursor.<handle>` — read-offset cache; the consumer could track it
  client-side, the file is just convenience.

There's no registry, no permission model, no schema beyond the JSONL
envelope. `rm -rf` everything but the logs and switchboard reconstructs
equivalent behavior on next run.

### Why no daemon

The design rejects unix sockets, in-process pub/sub, and an MCP server
for one reason: they fix transport problems we don't have at
3-session × 200-char/sec scale, and they cost the cat-debuggability
property immediately. The file is the protocol; the moment a daemon sits
in front of it, the daemon becomes the protocol and the file is just
storage.

### Cache, not /tmp

Default `$SWITCHBOARD_DIR` is `~/.cache/switchboard/`. This survives
reboots — important because Claude sessions can compact and lose
in-conversation context, and the log is the only durable record of prior
coordination. `/tmp` would erase that on boot. macOS will rotate
`~/.cache` over time, which is the right kind of decay.

For ephemeral test runs, override: `SWITCHBOARD_DIR=$(mktemp -d)`.

### Operational rule: never edit the live log in place

Don't `sed -i`, `> log`, `truncate`, or rename-replace the active log.
macOS `sed -i ''` writes-and-renames, which changes the inode; every
running `stream` detects the inode change, emits `kind:"rotated"`, and
re-reads from offset 0. Surprising for consumers.

If you must reset state: stop all subscribers, `rm -rf $SWITCHBOARD_DIR/<ch>`,
let it be re-created.

## Etiquette

- Pick a self-describing handle on first send (encodes your role).
- Multi-line bodies: prefer `send -` (stdin via heredoc) or `send -f
  <file>` to avoid shell quoting pain on long bodies.
- Use `switchboard log --since <ts>` rather than asking a peer to repeat
  themselves.
- Keep messages dense; the medium rewards it.

## License

Whatever sinisa decides. Intended for personal/internal use.
