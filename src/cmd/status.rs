use anyhow::Result;
use chrono::Utc;
use std::io::Write;

use crate::paths::{Env, PEER_STALE_SECS};
use crate::stream::roster;

pub fn run(env: &Env, handle: Option<&str>, channel: &str) -> Result<()> {
    let mut out = std::io::stdout().lock();

    writeln!(out, "dir:     {}", env.root().display())?;
    let handle_str = handle.unwrap_or("(none)");
    writeln!(out, "handle:  {handle_str}")?;
    writeln!(out, "channel: {channel}")?;

    let members = roster(env, channel)?;
    let now = Utc::now();
    let threshold = chrono::Duration::seconds(PEER_STALE_SECS as i64);
    let active = members.iter().filter(|m| now - m.last_seen < threshold).count();
    let stale = members.len() - active;
    writeln!(out, "peers:   {active} active, {stale} stale")?;

    let log_path = env.log_path(channel);
    if log_path.exists() {
        let len = std::fs::metadata(&log_path)?.len();
        writeln!(out, "log:     {} bytes", len)?;
    } else {
        writeln!(out, "log:     (none)")?;
    }

    if let Some(h) = handle {
        let cursor_path = env.cursor_path(channel, h);
        if cursor_path.exists() {
            let val = std::fs::read_to_string(&cursor_path)?;
            writeln!(out, "cursor:  {}", val.trim())?;
        } else {
            writeln!(out, "cursor:  (none)")?;
        }
    }

    Ok(())
}
