use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::io::Write;
use std::time::{Duration, SystemTime};

use crate::paths::{Env, PEER_STALE_SECS};

#[derive(Serialize)]
struct PeerInfo {
    handle: String,
    last_seen: DateTime<Utc>,
    stale: bool,
}

pub fn run(env: &Env, channel: &str) -> Result<()> {
    let peers_dir = env.peers_dir(channel);
    if !peers_dir.exists() {
        eprintln!("no peers on this channel — verify SWITCHBOARD_DIR if expecting peers");
        return Ok(());
    }
    let now = SystemTime::now();
    let threshold = Duration::from_secs(PEER_STALE_SECS);
    let mut entries = vec![];
    for entry in std::fs::read_dir(&peers_dir)? {
        let entry = entry?;
        let name = match entry.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue,
        };
        let mtime = entry.metadata()?.modified()?;
        let stale = now.duration_since(mtime).unwrap_or(Duration::ZERO) > threshold;
        entries.push(PeerInfo {
            handle: name,
            last_seen: mtime.into(),
            stale,
        });
    }
    entries.sort_by(|a, b| a.handle.cmp(&b.handle));

    let mut stdout = std::io::stdout().lock();
    for e in &entries {
        serde_json::to_writer(&mut stdout, e)?;
        stdout.write_all(b"\n")?;
    }
    stdout.flush()?;

    if entries.is_empty() {
        eprintln!("no peers on this channel — verify SWITCHBOARD_DIR if expecting peers");
    }
    Ok(())
}
