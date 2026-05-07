use anyhow::Result;

use crate::io::{append_record, touch_peer};
use crate::paths::Env;
use crate::record::Record;

pub fn run(env: &Env, handle: &str, channel: &str) -> Result<()> {
    env.ensure_channel(channel)?;
    append_record(env, channel, &Record::resume(channel, handle))?;
    touch_peer(env, channel, handle)?;
    Ok(())
}
