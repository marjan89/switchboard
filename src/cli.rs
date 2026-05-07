use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};

use crate::cmd;
use crate::paths::Env;

#[derive(Parser)]
#[command(name = "switchboard", version, about = "File-based pub/sub for multi-Claude-session coordination")]
pub struct Cli {
    /// Handle to act as. Falls back to $SWITCHBOARD_NAME.
    #[arg(long, global = true)]
    pub handle: Option<String>,

    /// Channel to act on. Falls back to $SWITCHBOARD_CHANNEL, then "default".
    #[arg(long, global = true)]
    pub channel: Option<String>,

    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Append a message. Auto-creates the channel and emits kind:join on first send.
    Send {
        /// Comma-separated targets. Omit for broadcast.
        #[arg(long)]
        to: Option<String>,

        /// Body. Use "-" for stdin, or pair with -f to read from a file.
        #[arg(short = 'f', long = "file", value_name = "FILE")]
        file: Option<String>,

        /// Body words (joined with spaces). Use "-" to read stdin instead.
        body: Vec<String>,
    },

    /// Emit kind:leave and remove peers/<handle>.
    Leave,

    /// Long-running JSONL firehose. Emits kind:roster, kind:ready, then live tail.
    Stream {
        /// Watch every channel under $SWITCHBOARD_DIR (current and future).
        #[arg(long, conflicts_with = "channel")]
        all: bool,

        /// Replay the entire log before going live.
        #[arg(long, conflicts_with = "from_cursor")]
        from_start: bool,

        /// Resume from cursor.<handle>; advance as records flow.
        #[arg(long)]
        from_cursor: bool,

        /// Suppress records where from == your handle.
        #[arg(long)]
        exclude_self: bool,

        /// Only emit records with this kind (e.g. message, service_announcement).
        #[arg(long, value_name = "KIND")]
        kind: Option<String>,
    },

    /// One-shot replay of the channel log with optional filters (no follow).
    Log {
        /// Only records with ts >= this UTC ISO 8601 timestamp.
        #[arg(long)]
        since: Option<String>,

        /// Only records with this kind.
        #[arg(long)]
        kind: Option<String>,

        /// Only records authored by this handle.
        #[arg(long)]
        from: Option<String>,

        /// Only the last N matching records.
        #[arg(long)]
        last: Option<usize>,
    },

    /// List channels as JSONL: {ch, peers_active, last_event_ts}.
    Channels {
        /// Include channels with no active peers.
        #[arg(long)]
        all: bool,
    },

    /// List peers in the channel as JSONL: {handle, last_seen, stale}.
    Peers,

    /// One-shot pull: emit records since cursor, then advance cursor to EOF.
    Recv,

    /// Show connection status: handle, channel, peer count, log size, cursor.
    Status,

    /// Remove stale peer files (mtime older than PEER_STALE_SECS).
    Prune,

    /// Advance cursor.<handle> to current EOF (drop pending backlog).
    MarkRead,
}

pub fn dispatch(cli: Cli) -> Result<()> {
    let env = Env::resolve(cli.handle.clone(), cli.channel.clone())?;
    match cli.cmd {
        Cmd::Send { to, file, body } => {
            let handle = env.require_handle()?;
            let channel = env.channel();
            let to = to.map(parse_to_list).unwrap_or_default();
            let body = read_body(body, file.as_deref())?;
            cmd::send::run(&env, handle, channel, to, body)
        }
        Cmd::Leave => {
            let handle = env.require_handle()?;
            let channel = env.channel();
            cmd::leave::run(&env, handle, channel)
        }
        Cmd::Stream { all, from_start, from_cursor, exclude_self, kind } => {
            let handle = env.handle().map(String::from);
            if from_cursor && handle.is_none() {
                return Err(anyhow!("--from-cursor requires --handle or $SWITCHBOARD_NAME"));
            }
            if exclude_self && handle.is_none() {
                return Err(anyhow!("--exclude-self requires --handle or $SWITCHBOARD_NAME"));
            }
            let scope = if all {
                cmd::stream::Scope::All
            } else {
                cmd::stream::Scope::One(env.channel().to_string())
            };
            let start = if from_start {
                cmd::stream::Start::FromStart
            } else if from_cursor {
                cmd::stream::Start::FromCursor(handle.clone().unwrap())
            } else {
                cmd::stream::Start::FromEof
            };
            let filter = cmd::stream::Filter {
                exclude_self: if exclude_self { handle.clone() } else { None },
                kind,
            };
            cmd::stream::run(&env, scope, start, handle, filter)
        }
        Cmd::Log { since, kind, from, last } => {
            let channel = env.channel();
            cmd::log::run(&env, channel, since.as_deref(), kind.as_deref(), from.as_deref(), last)
        }
        Cmd::Channels { all } => cmd::channels::run(&env, all),
        Cmd::Peers => {
            let channel = env.channel();
            cmd::peers::run(&env, channel)
        }
        Cmd::Recv => {
            let handle = env.require_handle()?;
            let channel = env.channel();
            cmd::recv::run(&env, handle, channel)
        }
        Cmd::Status => {
            let handle = env.handle();
            let channel = env.channel();
            cmd::status::run(&env, handle, channel)
        }
        Cmd::Prune => {
            let channel = env.channel();
            cmd::prune::run(&env, channel)
        }
        Cmd::MarkRead => {
            let handle = env.require_handle()?;
            let channel = env.channel();
            cmd::mark_read::run(&env, handle, channel)
        }
    }
}

fn parse_to_list(raw: String) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn read_body(positional: Vec<String>, file: Option<&str>) -> Result<String> {
    if let Some(path) = file {
        return std::fs::read_to_string(path)
            .with_context(|| format!("read body from {path}"));
    }
    if positional.len() == 1 && positional[0] == "-" {
        return read_stdin();
    }
    if positional.is_empty() {
        if atty::isnt(atty::Stream::Stdin) {
            return read_stdin();
        }
        return Err(anyhow!("empty body. pass words, '-' for stdin, or -f <file>"));
    }
    Ok(positional.join(" "))
}

fn read_stdin() -> Result<String> {
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
        .context("read body from stdin")?;
    let trimmed = buf.trim_end().to_string();
    if trimmed.is_empty() {
        return Err(anyhow!("empty body from stdin"));
    }
    Ok(trimmed)
}
