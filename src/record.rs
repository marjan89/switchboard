use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub ts: DateTime<Utc>,
    pub ch: String,
    pub kind: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<RosterMember>,

    /// Stamped on `kind:"join"` so subscribers (notably bots) can locate the
    /// joiner's Claude Code transcript dir without participant cooperation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Set on `kind:"service_announcement"` by the emitter (e.g. a bot)
    /// to signal severity. Typical values: "warning", "critical".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RosterMember {
    pub handle: String,
    pub last_seen: DateTime<Utc>,
}

impl Record {
    pub fn message(ch: &str, from: &str, to: Vec<String>, body: String) -> Self {
        Self {
            ts: Utc::now(),
            ch: ch.to_string(),
            kind: "message".to_string(),
            from: Some(from.to_string()),
            to,
            body: Some(body),
            handle: None,
            members: vec![],
            cwd: None,
            level: None,
        }
    }

    /// Loud-voice warning, conventionally emitted by bots. Carries body text
    /// and a level ("warning" / "critical") so consumers can route or style.
    pub fn service_announcement(ch: &str, from: &str, level: &str, body: String) -> Self {
        Self {
            ts: Utc::now(),
            ch: ch.to_string(),
            kind: "service_announcement".to_string(),
            from: Some(from.to_string()),
            to: vec![],
            body: Some(body),
            handle: None,
            members: vec![],
            cwd: None,
            level: Some(level.to_string()),
        }
    }

    pub fn join(ch: &str, handle: &str, cwd: Option<String>) -> Self {
        Self {
            ts: Utc::now(),
            ch: ch.to_string(),
            kind: "join".to_string(),
            from: None,
            to: vec![],
            body: None,
            handle: Some(handle.to_string()),
            members: vec![],
            cwd,
            level: None,
        }
    }

    pub fn leave(ch: &str, handle: &str) -> Self {
        Self::system_event(ch, "leave", Some(handle.to_string()))
    }

    pub fn roster(ch: &str, members: Vec<RosterMember>) -> Self {
        let mut rec = Self::system_event(ch, "roster", None);
        rec.members = members;
        rec
    }

    pub fn ready(ch: &str) -> Self {
        Self::system_event(ch, "ready", None)
    }

    pub fn rotated(ch: &str) -> Self {
        Self::system_event(ch, "rotated", None)
    }

    fn system_event(ch: &str, kind: &str, handle: Option<String>) -> Self {
        Self {
            ts: Utc::now(),
            ch: ch.to_string(),
            kind: kind.to_string(),
            from: None,
            to: vec![],
            body: None,
            handle,
            members: vec![],
            cwd: None,
            level: None,
        }
    }
}

pub fn write_jsonl<W: std::io::Write>(w: &mut W, rec: &Record) -> std::io::Result<()> {
    serde_json::to_writer(&mut *w, rec)?;
    w.write_all(b"\n")?;
    Ok(())
}
