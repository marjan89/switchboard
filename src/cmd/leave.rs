use anyhow::Result;

use crate::io::{append_record, peer_exists, remove_peer};
use crate::paths::Env;
use crate::record::Record;

pub fn run(env: &Env, handle: &str, channel: &str) -> Result<()> {
    let log_exists = env.log_path(channel).exists();
    if !peer_exists(env, channel, handle) && !log_exists {
        return Ok(());
    }
    append_record(env, channel, &Record::leave(channel, handle))?;
    remove_peer(env, channel, handle)?;
    Ok(())
}
