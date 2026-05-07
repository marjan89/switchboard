# switchboard-token-watcher

Switchboard bot. Subscribes to one or more channels and warns participants
when their context-window utilization crosses configurable thresholds, so
they can `/compact` before overrunning.

A separate binary in the switchboard workspace, not a subcommand of
`switchboard` itself. Bots are participants, not core; this layout keeps
switchboard's surface narrow and sets the precedent that future bots
(LLM-driven or otherwise) live alongside as peers, not as embedded
features.

## Trust model

The bot **trusts no one**. It establishes everything from two ground-truth
feeds:

1. **The switchboard channel log.** Each `kind:"join"` carries the
   participant's `cwd` (stamped by switchboard itself, not by the
   participant), so the bot knows where each handle's Claude Code
   project dir lives.
2. **The Claude Code transcripts** under `~/.claude/projects/<encoded-cwd>/`.
   The bot reads the latest `assistant` turn's `message.usage` directly
   to compute input footprint (`input_tokens + cache_creation +
   cache_read`) and `message.model` for the context window lookup.

No participant-side wrapper, no service announcement, no handshake.
Participants just `switchboard send` and the bot figures them out.

## Voices

- **`kind:"service_announcement"`** — the loud voice. Used when shit
  gets real (a participant has crossed a threshold). `level` field
  carries severity (`warning` / `critical`).
- **`kind:"message"`** — the casual voice, used for periodic sitreps
  when `--sitrep <secs>` is set. Body shape: `sitrep: alice 42% (opus-4-7), bob 67% (sonnet-4-6)`.

## Defaults

| param | default |
|---|---|
| handle | `bot-token-watcher` |
| thresholds | `0.8`, `0.9`, `0.95` |
| poll interval | `5` seconds |
| sitrep cadence | off (set `--sitrep <secs>` to enable) |
| context window | `200000` (any model not overridden) |
| level | `warning` for thresholds < 0.9, `critical` otherwise |

## Run

```sh
# watch one channel, default thresholds
switchboard-token-watcher --channel default

# watch every channel, current and future
switchboard-token-watcher --all

# custom thresholds, faster poll, sitreps every minute
switchboard-token-watcher --channel ops \
    --threshold 0.7 --threshold 0.85 --threshold 0.95 \
    --poll 2 \
    --sitrep 60 \
    --handle bot-tw-ops

# override a model's window
switchboard-token-watcher --channel default \
    --context-window claude-foo=180000
```

The bot is **headless by design** — no terminal, no TTY, no Claude session.
Run modes are deploy-time choices:

- **Foreground** (debugging): runs in the terminal, stderr shows startup
  banner + any append failures.
- **Backgrounded**: `&` + redirect stderr.
- **launchd** (recommended for always-on): a `.plist` under
  `~/Library/LaunchAgents/`, auto-restart on crash. Plist generation is a
  bootstrap concern; not shipped here.
- **tmux detached pane**: dedicate one pane per bot.

## Threshold semantics

Thresholds are sorted ascending. The bot tracks per (handle, channel)
which thresholds it has already warned on. On each poll:

1. If the participant's footprint has dropped **below the lowest
   threshold**, all `crossed` state for that handle is cleared (compaction
   observed → re-arm).
2. Otherwise, find the highest threshold the footprint has crossed but
   not yet been warned on. Emit one warning at that level. Mark that level
   *and all lower levels* as crossed, so a single jump from 50% to 95%
   produces one warning (the critical one), not three.

This trades verbosity for signal — sinisa wants to know "you're running
out of room," not the same fact three times.

## Failure modes

- **Participants run in cwds with no Claude Code transcripts** (e.g. ad-hoc
  shells): no jsonl in the encoded project dir → bot can't measure → no
  warnings. Silently skipped.
- **Multiple Claude sessions in the same cwd**: ambiguous — bot picks the
  most-recently-modified `.jsonl` in that dir, so it tracks the most
  recently active session. Run sessions from distinct cwds for clean
  attribution.
- **Bot killed without sending leave**: `peers/bot-token-watcher` stays
  until the 5-min stale threshold ages it out. Same as any participant
  that crashes.
- **Multiple bots on the same channel**: harmless; each tracks its own
  state. Distinct `--handle` keeps warnings attributable.

## Layout

```
bots/token-watcher/
  Cargo.toml         (depends on switchboard workspace member)
  src/main.rs        (the bot loop — clap parsing, drain+poll, warning, sitrep)
  tests/cli.rs       (7 integration tests via assert_cmd)
  README.md
```
