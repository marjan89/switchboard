//! switchboard library crate. Exposes wire-format types, filesystem
//! helpers, and the CLI dispatcher. Workspace bots depend on this crate
//! for typed access instead of shelling out to the CLI.

pub mod cli;
pub mod cmd;
pub mod io;
pub mod paths;
pub mod record;
pub mod stream;
