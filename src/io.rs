use anyhow::{Context, Result, bail};
use std::fs::OpenOptions;
use std::io::Write;
use std::time::SystemTime;

use crate::paths::{Env, MAX_BODY_BYTES};
use crate::record::{Record, write_jsonl};

/// Append a single JSONL record to the channel's log. Atomic at the macOS
/// PIPE_BUF boundary; bodies are capped at MAX_BODY_BYTES to keep writes
/// under the guarantee.
pub fn append_record(env: &Env, channel: &str, rec: &Record) -> Result<()> {
    if let Some(b) = &rec.body {
        if b.len() > MAX_BODY_BYTES {
            bail!("body exceeds {MAX_BODY_BYTES}-byte cap (got {} bytes)", b.len());
        }
    }
    env.ensure_channel(channel)?;
    let log_path = env.log_path(channel);
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    write_jsonl(&mut buf, rec)?;
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open {} for append", log_path.display()))?;
    f.write_all(&buf)
        .with_context(|| format!("append to {}", log_path.display()))?;
    Ok(())
}

/// Touch peers/<handle> (mtime = now). Creates the file if it doesn't exist.
pub fn touch_peer(env: &Env, channel: &str, handle: &str) -> Result<()> {
    let path = env.peer_file(channel, handle);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        let f = OpenOptions::new().write(true).open(&path)?;
        f.set_modified(SystemTime::now())?;
    } else {
        std::fs::File::create(&path)?;
    }
    Ok(())
}

/// True if peers/<handle> exists.
pub fn peer_exists(env: &Env, channel: &str, handle: &str) -> bool {
    env.peer_file(channel, handle).exists()
}

/// Remove peers/<handle> if it exists.
pub fn remove_peer(env: &Env, channel: &str, handle: &str) -> Result<()> {
    let path = env.peer_file(channel, handle);
    if path.exists() {
        std::fs::remove_file(&path)
            .with_context(|| format!("remove {}", path.display()))?;
    }
    Ok(())
}

