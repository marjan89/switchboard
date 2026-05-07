use anyhow::{Result, anyhow};

use crate::io::{append_record, peer_exists, touch_peer};
use crate::paths::{Env, MAX_BODY_BYTES};
use crate::record::Record;

const CHUNK_OVERHEAD: usize = 20; // "(NN/NN) " prefix + margin

pub fn run(env: &Env, handle: &str, channel: &str, to: Vec<String>, body: String) -> Result<()> {
    if body.is_empty() {
        return Err(anyhow!("empty body"));
    }

    let first_send = !peer_exists(env, channel, handle);
    if first_send {
        let cwd = std::env::current_dir()
            .ok()
            .and_then(|p| p.to_str().map(str::to_string));
        append_record(env, channel, &Record::join(channel, handle, cwd))?;
    }

    if body.len() <= MAX_BODY_BYTES {
        append_record(env, channel, &Record::message(channel, handle, to, body))?;
    } else {
        let chunk_size = MAX_BODY_BYTES - CHUNK_OVERHEAD;
        let chunks = chunk_body(&body, chunk_size);
        let total = chunks.len();
        for (i, chunk) in chunks.into_iter().enumerate() {
            let tagged = format!("({}/{}) {}", i + 1, total, chunk);
            append_record(
                env,
                channel,
                &Record::message(channel, handle, to.clone(), tagged),
            )?;
        }
    }

    touch_peer(env, channel, handle)?;
    Ok(())
}

fn chunk_body(body: &str, max: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut start = 0;
    let bytes = body.as_bytes();
    while start < bytes.len() {
        let mut end = (start + max).min(bytes.len());
        if end < bytes.len() {
            while end > start && !body.is_char_boundary(end) {
                end -= 1;
            }
            let slice = &body[start..end];
            if let Some(nl) = slice.rfind('\n') {
                end = start + nl + 1;
            }
        }
        chunks.push(body[start..end].to_string());
        start = end;
    }
    chunks
}
