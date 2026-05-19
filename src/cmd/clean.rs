use anyhow::{Context, Result};
use std::time::{Duration, SystemTime};

use crate::io::append_record;
use crate::paths::{Env, PEER_STALE_SECS};
use crate::record::Record;

pub fn run(env: &Env, channel: &str, full: bool, keep_log: bool) -> Result<()> {
    let ch_dir = env.channel_dir(channel);
    if !ch_dir.exists() {
        println!("channel {channel} does not exist; nothing to clean");
        return Ok(());
    }

    if full {
        std::fs::remove_dir_all(&ch_dir)
            .with_context(|| format!("remove {}", ch_dir.display()))?;
        println!("removed channel directory {}", ch_dir.display());
        return Ok(());
    }

    // 1. Prune stale peers
    let peers_dir = env.peers_dir(channel);
    if peers_dir.exists() {
        let threshold = Duration::from_secs(PEER_STALE_SECS);
        let now = SystemTime::now();
        for entry in std::fs::read_dir(&peers_dir)
            .with_context(|| format!("read {}", peers_dir.display()))?
        {
            let entry = entry?;
            let name = entry.file_name();
            let Some(handle) = name.to_str() else { continue };
            let meta = entry.metadata()?;
            let age = now
                .duration_since(meta.modified()?)
                .unwrap_or(Duration::ZERO);
            if age > threshold {
                std::fs::remove_file(entry.path())?;
                println!("pruned {handle} (stale {}s)", age.as_secs());
            }
        }
    }

    // 2. Reset all cursors
    for entry in std::fs::read_dir(&ch_dir)
        .with_context(|| format!("read {}", ch_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else { continue };
        if name_str.starts_with("cursor.") {
            std::fs::remove_file(entry.path())?;
            println!("reset {name_str}");
        }
    }

    // 3. Rotate the log (unless --keep-log)
    if !keep_log {
        let log_path = env.log_path(channel);
        if log_path.exists() {
            let backup = ch_dir.join("log.1");
            std::fs::rename(&log_path, &backup)
                .with_context(|| format!("rotate {}", log_path.display()))?;
            println!("rotated log → log.1");

            // Write kind:"rotated" into the fresh log so active subscribers pick it up
            append_record(env, channel, &Record::rotated(channel))?;
        }
    }

    Ok(())
}
