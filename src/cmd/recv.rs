use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};

use crate::paths::Env;

pub fn run(env: &Env, handle: &str, channel: &str) -> Result<()> {
    let log_path = env.log_path(channel);
    if !log_path.exists() {
        return Ok(());
    }

    let cursor_path = env.cursor_path(channel, handle);
    let offset: u64 = std::fs::read_to_string(&cursor_path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);

    let f = File::open(&log_path)
        .with_context(|| format!("open {}", log_path.display()))?;
    let len = f.metadata()?.len();
    if offset >= len {
        return Ok(());
    }

    let mut reader = BufReader::new(f);
    reader.seek(SeekFrom::Start(offset))?;

    let mut stdout = std::io::stdout().lock();
    for line in reader.lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        stdout.write_all(line.as_bytes())?;
        stdout.write_all(b"\n")?;
    }
    stdout.flush()?;

    if let Some(parent) = cursor_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut cf = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&cursor_path)
        .with_context(|| format!("open {}", cursor_path.display()))?;
    write!(cf, "{len}")?;

    Ok(())
}
