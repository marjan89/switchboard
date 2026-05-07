use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::io::Write;

use crate::paths::Env;
use crate::stream::roster;

#[derive(Serialize)]
struct ChannelInfo {
    ch: String,
    peers_active: usize,
    last_event_ts: Option<DateTime<Utc>>,
}

pub fn run(env: &Env, all: bool) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    for ch in env.list_channels()? {
        let active = roster(env, &ch)?;
        let last_event_ts = last_event_ts(env, &ch);

        if !all && active.is_empty() {
            continue;
        }

        let info = ChannelInfo {
            ch,
            peers_active: active.len(),
            last_event_ts,
        };
        serde_json::to_writer(&mut stdout, &info)?;
        stdout.write_all(b"\n")?;
    }
    stdout.flush()?;
    Ok(())
}

fn last_event_ts(env: &Env, ch: &str) -> Option<DateTime<Utc>> {
    let log = env.log_path(ch);
    let meta = std::fs::metadata(&log).ok()?;
    let mtime = meta.modified().ok()?;
    Some(mtime.into())
}
