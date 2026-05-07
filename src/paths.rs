use anyhow::{Context, Result, anyhow};
use std::path::PathBuf;

pub const DEFAULT_CHANNEL: &str = "default";

/// Peer presence is "active" if its file's mtime is within this many seconds of now.
pub const PEER_STALE_SECS: u64 = 300;

/// Hard cap on a single message body to keep POSIX append atomic.
pub const MAX_BODY_BYTES: usize = 4096;

pub struct Env {
    root: PathBuf,
    handle: Option<String>,
    channel: String,
}

impl Env {
    pub fn resolve(handle_arg: Option<String>, channel_arg: Option<String>) -> Result<Self> {
        let root = match std::env::var_os("SWITCHBOARD_DIR") {
            Some(v) => PathBuf::from(v),
            None => {
                let cache = dirs::cache_dir()
                    .ok_or_else(|| anyhow!("could not resolve cache dir; set SWITCHBOARD_DIR"))?;
                cache.join("switchboard")
            }
        };
        let handle = handle_arg.or_else(|| std::env::var("SWITCHBOARD_NAME").ok())
            .filter(|s| !s.is_empty());
        let channel = channel_arg
            .or_else(|| std::env::var("SWITCHBOARD_CHANNEL").ok())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_CHANNEL.to_string());
        Ok(Self { root, handle, channel })
    }

    pub fn handle(&self) -> Option<&str> {
        self.handle.as_deref()
    }

    pub fn require_handle(&self) -> Result<&str> {
        self.handle
            .as_deref()
            .ok_or_else(|| anyhow!("handle required; set $SWITCHBOARD_NAME or pass --handle"))
    }

    pub fn channel(&self) -> &str {
        &self.channel
    }

    pub fn channel_dir(&self, ch: &str) -> PathBuf {
        self.root.join(ch)
    }

    pub fn log_path(&self, ch: &str) -> PathBuf {
        self.channel_dir(ch).join("log")
    }

    pub fn peers_dir(&self, ch: &str) -> PathBuf {
        self.channel_dir(ch).join("peers")
    }

    pub fn peer_file(&self, ch: &str, handle: &str) -> PathBuf {
        self.peers_dir(ch).join(handle)
    }

    pub fn cursor_path(&self, ch: &str, handle: &str) -> PathBuf {
        self.channel_dir(ch).join(format!("cursor.{handle}"))
    }

    pub fn ensure_channel(&self, ch: &str) -> Result<()> {
        let peers = self.peers_dir(ch);
        std::fs::create_dir_all(&peers)
            .with_context(|| format!("create channel dir {}", peers.display()))?;
        Ok(())
    }

    pub fn list_channels(&self) -> Result<Vec<String>> {
        if !self.root.exists() {
            return Ok(vec![]);
        }
        let mut out = vec![];
        for entry in std::fs::read_dir(&self.root)
            .with_context(|| format!("read {}", self.root.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                if name.starts_with('.') {
                    continue;
                }
                out.push(name.to_string());
            }
        }
        out.sort();
        Ok(out)
    }
}
