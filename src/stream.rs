use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::paths::Env;
use crate::record::Record;

/// Per-channel tail state. Tracks file offset and inode for rotation detection.
pub struct Tailer {
    pub channel: String,
    log_path: PathBuf,
    offset: u64,
    inode: u64,
    /// Optional cursor file path; offset is persisted here after each emitted record.
    cursor_path: Option<PathBuf>,
}

impl Tailer {
    /// Open at end-of-file (no backlog).
    pub fn at_eof(env: &Env, channel: &str) -> Result<Self> {
        let log_path = env.log_path(channel);
        let (offset, inode) = if log_path.exists() {
            let meta = std::fs::metadata(&log_path)?;
            (meta.len(), meta.ino())
        } else {
            (0, 0)
        };
        Ok(Self { channel: channel.to_string(), log_path, offset, inode, cursor_path: None })
    }

    /// Open at offset 0 (full backlog).
    pub fn at_start(env: &Env, channel: &str) -> Result<Self> {
        let log_path = env.log_path(channel);
        let inode = if log_path.exists() {
            std::fs::metadata(&log_path)?.ino()
        } else {
            0
        };
        Ok(Self { channel: channel.to_string(), log_path, offset: 0, inode, cursor_path: None })
    }

    /// Open at the cursor offset for the given handle. Cursor advances as records are emitted.
    pub fn at_cursor(env: &Env, channel: &str, handle: &str) -> Result<Self> {
        let log_path = env.log_path(channel);
        let cursor_path = env.cursor_path(channel, handle);
        let offset = if cursor_path.exists() {
            std::fs::read_to_string(&cursor_path)
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(0)
        } else {
            0
        };
        let inode = if log_path.exists() {
            std::fs::metadata(&log_path)?.ino()
        } else {
            0
        };
        Ok(Self {
            channel: channel.to_string(),
            log_path,
            offset,
            inode,
            cursor_path: Some(cursor_path),
        })
    }

    /// Read any new records and emit each via the callback. Returns rotated=true if the file's
    /// inode changed since the last read; caller should emit a kind:rotated record.
    pub fn drain<F>(&mut self, mut emit: F) -> Result<bool>
    where
        F: FnMut(&Record) -> std::io::Result<()>,
    {
        if !self.log_path.exists() {
            return Ok(false);
        }
        let meta = std::fs::metadata(&self.log_path)?;
        let mut rotated = false;
        if self.inode != 0 && meta.ino() != self.inode {
            rotated = true;
            self.offset = 0;
        }
        self.inode = meta.ino();

        let len = meta.len();
        if len < self.offset {
            // truncated without inode change (e.g., reset). Treat as rotation.
            rotated = true;
            self.offset = 0;
        }
        if len == self.offset {
            return Ok(rotated);
        }

        let mut f = File::open(&self.log_path)
            .with_context(|| format!("open {}", self.log_path.display()))?;
        f.seek(SeekFrom::Start(self.offset))?;
        let mut reader = BufReader::new(f);
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line)?;
            if n == 0 {
                break;
            }
            // Skip incomplete trailing line (writer hasn't finished append).
            if !line.ends_with('\n') {
                break;
            }
            let trimmed = line.trim_end_matches('\n');
            if trimmed.is_empty() {
                self.offset += n as u64;
                continue;
            }
            match serde_json::from_str::<Record>(trimmed) {
                Ok(rec) => {
                    emit(&rec)?;
                }
                Err(_) => {
                    // Malformed line — skip but advance offset so we don't loop on it.
                }
            }
            self.offset += n as u64;
            if let Some(cp) = &self.cursor_path {
                let _ = persist_cursor(cp, self.offset);
            }
        }
        Ok(rotated)
    }
}

fn persist_cursor(path: &Path, offset: u64) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).write(true).truncate(true).open(path)?;
    write!(f, "{offset}")?;
    Ok(())
}

/// Roster: peers active within PEER_STALE_SECS, derived from peers/<handle> mtimes.
pub fn roster(env: &Env, channel: &str) -> Result<Vec<crate::record::RosterMember>> {
    use chrono::{DateTime, Utc};
    use std::time::{Duration, SystemTime};

    let peers_dir = env.peers_dir(channel);
    if !peers_dir.exists() {
        return Ok(vec![]);
    }
    let now = SystemTime::now();
    let stale_threshold = Duration::from_secs(crate::paths::PEER_STALE_SECS);
    let mut members = vec![];
    for entry in std::fs::read_dir(&peers_dir)? {
        let entry = entry?;
        let name = match entry.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue,
        };
        let meta = entry.metadata()?;
        let mtime = meta.modified()?;
        if now.duration_since(mtime).unwrap_or(Duration::ZERO) > stale_threshold {
            continue;
        }
        let last_seen: DateTime<Utc> = mtime.into();
        members.push(crate::record::RosterMember { handle: name, last_seen });
    }
    members.sort_by(|a, b| a.handle.cmp(&b.handle));
    Ok(members)
}

