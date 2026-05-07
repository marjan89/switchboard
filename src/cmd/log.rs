use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use std::fs::File;
use std::io::{BufRead, BufReader, Write};

use crate::paths::Env;
use crate::record::Record;

pub fn run(
    env: &Env,
    channel: &str,
    since: Option<&str>,
    kind: Option<&str>,
    from: Option<&str>,
) -> Result<()> {
    let log_path = env.log_path(channel);
    if !log_path.exists() {
        return Ok(());
    }

    let since_ts: Option<DateTime<Utc>> = match since {
        Some(s) => Some(
            s.parse::<DateTime<Utc>>()
                .map_err(|e| anyhow!("--since must be RFC3339/ISO8601 UTC ({e})"))?,
        ),
        None => None,
    };

    let f = File::open(&log_path)
        .with_context(|| format!("open {}", log_path.display()))?;
    let reader = BufReader::new(f);
    let mut stdout = std::io::stdout().lock();
    for line in reader.lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let rec: Record = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if let Some(t) = since_ts {
            if rec.ts < t {
                continue;
            }
        }
        if let Some(k) = kind {
            if rec.kind != k {
                continue;
            }
        }
        if let Some(f) = from {
            let matches_from = rec.from.as_deref() == Some(f);
            let matches_handle = rec.handle.as_deref() == Some(f);
            if !matches_from && !matches_handle {
                continue;
            }
        }
        stdout.write_all(line.as_bytes())?;
        stdout.write_all(b"\n")?;
    }
    stdout.flush()?;
    Ok(())
}
