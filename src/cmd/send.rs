use anyhow::{Result, anyhow, bail};

use crate::io::{append_record, peer_exists, touch_peer};
use crate::paths::{Env, MAX_BODY_BYTES};
use crate::record::Record;

pub fn run(env: &Env, handle: &str, channel: &str, to: Vec<String>, body: String) -> Result<()> {
    if body.is_empty() {
        return Err(anyhow!("empty body"));
    }
    if body.len() > MAX_BODY_BYTES {
        bail!("body exceeds {MAX_BODY_BYTES}-byte cap (got {} bytes)", body.len());
    }

    let first_send = !peer_exists(env, channel, handle);
    if first_send {
        let cwd = std::env::current_dir()
            .ok()
            .and_then(|p| p.to_str().map(str::to_string));
        append_record(env, channel, &Record::join(channel, handle, cwd))?;
    }
    append_record(env, channel, &Record::message(channel, handle, to, body))?;
    touch_peer(env, channel, handle)?;
    Ok(())
}
