use anyhow::{Context, Result};
use std::fs::OpenOptions;
use std::io::Write;

use crate::paths::Env;

pub fn run(env: &Env, handle: &str, channel: &str) -> Result<()> {
    let log_path = env.log_path(channel);
    let cursor_path = env.cursor_path(channel, handle);

    if let Some(parent) = cursor_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let len = if log_path.exists() {
        std::fs::metadata(&log_path)?.len()
    } else {
        0
    };

    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&cursor_path)
        .with_context(|| format!("open {}", cursor_path.display()))?;
    write!(f, "{len}")?;
    Ok(())
}
