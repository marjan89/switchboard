use anyhow::Result;

use crate::io::{append_record, peer_exists, remove_peer};
use crate::paths::Env;
use crate::record::Record;

pub fn run(env: &Env, handle: &str, channel: &str) -> Result<()> {
    if !peer_exists(env, channel, handle) {
        return Ok(());
    }
    append_record(env, channel, &Record::leave(channel, handle))?;
    remove_peer(env, channel, handle)?;
    Ok(())
}
