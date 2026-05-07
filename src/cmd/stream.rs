use anyhow::Result;
use std::collections::HashMap;
use std::io::Write;
use std::time::Duration;

use crate::paths::Env;
use crate::record::Record;
use crate::stream::{Tailer, roster};

const POLL_INTERVAL: Duration = Duration::from_millis(150);

pub enum Scope {
    One(String),
    All,
}

#[derive(Clone)]
pub enum Start {
    FromEof,
    FromStart,
    FromCursor(String),
}

pub struct Filter {
    pub exclude_self: Option<String>,
    pub kind: Option<String>,
}

impl Filter {
    fn accept(&self, rec: &Record) -> bool {
        if let Some(ref me) = self.exclude_self {
            if rec.from.as_deref() == Some(me.as_str()) {
                return false;
            }
        }
        if let Some(ref k) = self.kind {
            if rec.kind != *k {
                return false;
            }
        }
        true
    }
}

pub fn run(env: &Env, scope: Scope, start: Start, _handle: Option<String>, filter: Filter) -> Result<()> {
    let mut tailers: HashMap<String, Tailer> = HashMap::new();

    let initial: Vec<String> = match &scope {
        Scope::One(ch) => vec![ch.clone()],
        Scope::All => env.list_channels()?,
    };

    eprintln!("switchboard stream: dir={}", env.root().display());

    let mut stdout = std::io::stdout().lock();

    for ch in &initial {
        bring_up(env, ch, &start, &filter, &mut tailers, &mut stdout)?;
    }

    loop {
        if matches!(scope, Scope::All) {
            for ch in env.list_channels()? {
                if !tailers.contains_key(&ch) {
                    bring_up(env, &ch, &start, &filter, &mut tailers, &mut stdout)?;
                }
            }
        }

        for tailer in tailers.values_mut() {
            let ch = tailer.channel.clone();
            let rotated = tailer.drain(|rec| {
                if filter.accept(rec) {
                    serde_json::to_writer(&mut stdout, rec)?;
                    stdout.write_all(b"\n")?;
                    stdout.flush()?;
                }
                Ok(())
            })?;
            if rotated {
                let rec = Record::rotated(&ch);
                if filter.accept(&rec) {
                    serde_json::to_writer(&mut stdout, &rec)?;
                    stdout.write_all(b"\n")?;
                    stdout.flush()?;
                }
            }
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Initialize a tailer for a channel and emit roster + (optional backlog) + ready in order.
fn bring_up<W: Write>(
    env: &Env,
    channel: &str,
    start: &Start,
    filter: &Filter,
    tailers: &mut HashMap<String, Tailer>,
    out: &mut W,
) -> Result<()> {
    env.ensure_channel(channel)?;

    // Roster reflects current presence at startup.
    let members = roster(env, channel)?;
    let rec = Record::roster(channel, members);
    serde_json::to_writer(&mut *out, &rec)?;
    out.write_all(b"\n")?;
    out.flush()?;

    // Open tailer at the requested position; drain backlog before going live.
    let mut tailer = match start {
        Start::FromEof => Tailer::at_eof(env, channel)?,
        Start::FromStart => Tailer::at_start(env, channel)?,
        Start::FromCursor(handle) => Tailer::at_cursor(env, channel, handle)?,
    };

    tailer.drain(|rec| {
        if filter.accept(rec) {
            serde_json::to_writer(&mut *out, rec)?;
            out.write_all(b"\n")?;
            out.flush()?;
        }
        Ok(())
    })?;

    // Boundary marker.
    let rec = Record::ready(channel);
    serde_json::to_writer(&mut *out, &rec)?;
    out.write_all(b"\n")?;
    out.flush()?;

    tailers.insert(channel.to_string(), tailer);
    Ok(())
}
