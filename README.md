# switchboard

File-based pub/sub for multi-Claude-session coordination. One shared
append-only log, one cursor per subscriber. No daemon, no sockets, no auth.

## What it's for

When multiple Claude Code sessions need to coordinate on a shared task —
a unified dashboard, a schema migration spread across two repos, an
integration where one session has the generator and another has the
renderer — switchboard gives them a channel to talk. The medium itself
turns out to be unusually load-bearing: append-only + tone-blind +
no-edit-button forces tighter writing than any chat tool.

The first real use of this produced a v1.0 schema, JSON payload,
self-rendering dashboard, and `bin/onboard-skill` ceremony in 40 minutes
across three Claude sessions and a fourth observer-translator. None of
them swore. Signal-to-noise hit ~85:15.

## Install

```sh
git clone git@github.com:marjan89/switchboard.git /Users/Shared/projects/substrate-distro/switchboard
ln -sf /Users/Shared/projects/substrate-distro/switchboard/src/switchboard /opt/homebrew/bin/switchboard
```

Verify:

```sh
SWITCHBOARD_NAME=test switchboard 2>&1 | head -3
# usage: SWITCHBOARD_NAME=<handle> switchboard <cmd>
```

## Layout

```
switchboard/
├── README.md       # this file — architecture and design rationale
└── src/
    └── switchboard # the binary (bash for now; see "Future direction")
```

Runtime artifacts (the conversation log and per-subscriber cursors) live
at `/tmp/switchboard/{log, cursor.<NAME>}` — ephemeral by design.
Override the directory via `SWITCHBOARD_DIR` env var if needed for tests.

## Architecture

### The file IS the protocol

Three load-bearing properties fall out of "shared append-only file +
per-subscriber cursor":

1. **Append-only kills write-races** at the macOS atomic-write boundary.
   Two senders writing the same instant produce two complete lines, never
   a half-line.
2. **`cat` is the inspector.** No daemon to query, no socket to introspect.
   The log is the system of record. Recovery on any process death is "the
   file is still there."
3. **Plain text is debuggable.** `grep`, `awk`, `tail -F`, `sed` all work.
   Versioning, diffing, archiving — all free.

### Mechanism

**`send`** appends `[HH:MM:SS] name: <line>` to the log. Multi-line bodies
get every line stamped with the same prefix so line-based filters work
across the full message body — without this, a `grep -v ' name:'` would
leak continuation lines.

**`recv`** reads from the per-subscriber cursor offset to EOF, then
advances the cursor. Non-blocking, idempotent across crashes. Pull-style.

**`subscribe`** emits a `tail -n0 -F LOG | grep --line-buffered -v ' name:'`
pipeline. Pipe it into a stdout-streaming notification system (e.g.
Claude Code's Monitor) for push-style delivery. `tail -n0` deliberately
*excludes* the backlog — fresh subscribers get the present, not 500 lines
of past context flooding their session. Use `search` for on-demand history.

**`search`** runs `grep -nE` over the log with line numbers. The
intentional asymmetry: subscribe gives you the present, search/log give
you the past on demand. New joiners look stuff up when something
references prior context they don't recognize, rather than absorbing
hundreds of lines they may not need.

### Why no daemon

Considered Unix domain sockets, in-process pub/sub, MCP server. Rejected
all three at v1 for one reason: they fix transport problems we don't have
at 3-session × 200-char/sec scale, and they cost the cat-debuggability
property immediately. The file is the protocol; once you put a daemon in
front of it, the daemon becomes the protocol and the file is just storage.

The escalation path, if it's ever needed:

- **v1 (now):** bash + file
- **v2:** Go binary, same file-based protocol, better argv/cross-platform
- **v3 (only if v2 hits a ceiling):** MCP server exposing
  `switchboard_recv` / `switchboard_subscribe` / `switchboard_send` as
  native Claude tools — sidesteps the `tail -F | grep` Monitor hack with
  a typed schema. Skip the unix-socket layer entirely; it's the awkward
  middle.

## Conversation log location

The log is canonically at:

```
/tmp/switchboard/log
```

Override via `SWITCHBOARD_DIR=/some/other/path` in the environment if
needed for testing or per-channel isolation.

`/tmp/` is the **right** location, not a default-because-we-haven't-decided.
Switchboard state is meant to be ephemeral — yesterday's coordination
thread shouldn't haunt tomorrow's session. Survival across reboots is an
**anti-feature**: a fresh boot means a fresh channel, which is the
cleanest possible scope reset.

## Operational rule: never edit the live log in place

`/tmp/switchboard/log` is append-only at runtime. **Do not** edit it via
`sed -i`, `> log` redirection, `truncate`, or any rename-and-replace
operation. macOS `sed -i ''` writes to a temp file and renames over the
original, which changes the inode — every active `tail -F` subscriber
detects "file replaced," re-opens the new inode, and replays the entire
log into their notification stream. Multiple Claude sessions get tens of
KB of history dumped into their context.

The only sanctioned mutation is `switchboard reset` (atomic truncate via
`: > $LOG`, also clears cursors). If you need to remove a specific
message — don't. Name entries so they're recognizable as test/junk
entries and let them age out of relevance. The log is the system of
record; rewriting it is anti-pattern.

If you absolutely must clean state: stop all subscribers first, then
`reset`, then accept that you've cleared everything.

## Etiquette

- Identify yourself in the first message of any channel
- Call `recv` (or rely on a Monitor subscription) before `send` so you
  see the latest before replying
- Multi-line messages: always `send -` (stdin via heredoc) or `send -f
  <file>`. Direct argv truncates and breaks on shell quoting for long
  bodies.
- If a message references something unfamiliar, use `switchboard search
  <pattern>` rather than asking — the answer is probably in the log
- Keep messages dense; the medium rewards it

## Commands

```
switchboard send <msg>          append a message
switchboard send -              read body from stdin (multi-line)
switchboard send -f <file>      read body from file
switchboard recv                pull: messages since last recv
switchboard subscribe           push: emit tail+grep cmd for Monitor
switchboard search <pattern>    grep history (on-demand lookup)
switchboard log                 full transcript
switchboard mark-read           skip current backlog
switchboard reset               wipe the channel (use sparingly)
```

## License

Whatever sinisa decides. Intended for personal/internal use.
