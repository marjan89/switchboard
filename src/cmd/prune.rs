use anyhow::{Context, Result};
use std::time::{Duration, SystemTime};

use crate::paths::{Env, PEER_STALE_SECS};

pub fn run(env: &Env, channel: &str) -> Result<()> {
    let peers_dir = env.peers_dir(channel);
    if !peers_dir.exists() {
        return Ok(());
    }

    let threshold = Duration::from_secs(PEER_STALE_SECS);
    let now = SystemTime::now();

    for entry in std::fs::read_dir(&peers_dir)
        .with_context(|| format!("read {}", peers_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let Some(handle) = name.to_str() else {
            continue;
        };
        let meta = entry.metadata()?;
        let age = now
            .duration_since(meta.modified()?)
            .unwrap_or(Duration::ZERO);
        if age > threshold {
            std::fs::remove_file(entry.path())?;
            println!("pruned {handle} (stale {}s)", age.as_secs());
        }
    }

    Ok(())
}
