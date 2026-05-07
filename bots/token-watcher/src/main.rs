mod daemon;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use switchboard::io::{append_record, peer_exists, remove_peer, touch_peer};
use switchboard::paths::{Env, PEER_STALE_SECS};
use switchboard::record::Record;
use switchboard::stream::Tailer;

const DEFAULT_HANDLE: &str = "bot-token-watcher";
const DEFAULT_POLL_SECS: u64 = 5;
const DRAIN_INTERVAL: Duration = Duration::from_millis(150);

struct ThresholdRung {
    ratio: f64,
    level: &'static str,
    action: &'static str,
}

const MULTI_AGENT_DEFAULTS: &[ThresholdRung] = &[
    ThresholdRung { ratio: 0.25, level: "warning",  action: "consider /compact" },
    ThresholdRung { ratio: 0.40, level: "alert",    action: "compact now" },
    ThresholdRung { ratio: 0.50, level: "critical", action: "bleeding tokens" },
];

const SINGLE_AGENT_DEFAULTS: &[ThresholdRung] = &[
    ThresholdRung { ratio: 0.50, level: "warning",  action: "consider /compact" },
    ThresholdRung { ratio: 0.60, level: "alert",    action: "compact now" },
    ThresholdRung { ratio: 0.70, level: "critical", action: "bleeding tokens" },
];

const LEVEL_PROGRESSION: &[(&str, &str)] = &[
    ("warning",  "consider /compact"),
    ("alert",    "compact now"),
    ("critical", "bleeding tokens"),
];

const DEFAULT_WINDOW: u64 = 200_000;

const MODEL_WINDOWS: &[(&str, u64)] = &[
    ("claude-opus-4-7", 1_000_000),
    ("claude-opus-4-6", 200_000),
    ("claude-sonnet-4-6", 200_000),
];

fn default_context_window(model: &str) -> u64 {
    MODEL_WINDOWS
        .iter()
        .find(|(prefix, _)| model.starts_with(prefix))
        .map(|(_, w)| *w)
        .unwrap_or(DEFAULT_WINDOW)
}

#[derive(Parser)]
#[command(
    name = "switchboard-token-watcher",
    version,
    about = "Switchboard bot: warn participants approaching their model's context-window limit"
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the bot in the foreground.
    Run(RunArgs),
    /// Install and start as a launchd daemon.
    Start(RunArgs),
    /// Stop and uninstall the launchd daemon.
    Stop,
}

#[derive(clap::Args)]
struct RunArgs {
    /// Channels to watch. Repeat for multiple. Mutually exclusive with --all.
    #[arg(long)]
    channel: Vec<String>,

    /// Watch every channel, including future ones. Mutually exclusive with --channel.
    #[arg(long, conflicts_with = "channel")]
    all: bool,

    /// Threshold ratio (0.0-1.0). Repeat for multiple rungs. Overrides peer-count defaults.
    #[arg(long, value_parser = parse_threshold)]
    threshold: Vec<f64>,

    /// Transcript poll interval in seconds.
    #[arg(long, default_value_t = DEFAULT_POLL_SECS)]
    poll: u64,

    /// Sitrep cadence in seconds. Off when 0 (default).
    #[arg(long, default_value_t = 0)]
    sitrep: u64,

    /// Bot's switchboard handle.
    #[arg(long, default_value = DEFAULT_HANDLE)]
    handle: String,

    /// Override context window for a model: --context-window claude-foo=180000.
    #[arg(long, value_parser = parse_window_override)]
    context_window: Vec<(String, u64)>,

    /// Print correlation diagnostics to stderr.
    #[arg(long)]
    verbose: bool,
}

impl RunArgs {
    fn to_cli_args(&self) -> Vec<String> {
        let mut args = vec![];
        if self.all {
            args.push("--all".into());
        }
        for ch in &self.channel {
            args.extend(["--channel".into(), ch.clone()]);
        }
        for t in &self.threshold {
            args.extend(["--threshold".into(), t.to_string()]);
        }
        args.extend(["--poll".into(), self.poll.to_string()]);
        args.extend(["--sitrep".into(), self.sitrep.to_string()]);
        args.extend(["--handle".into(), self.handle.clone()]);
        for (k, v) in &self.context_window {
            args.extend(["--context-window".into(), format!("{k}={v}")]);
        }
        if self.verbose {
            args.push("--verbose".into());
        }
        args
    }
}

fn parse_threshold(s: &str) -> Result<f64, String> {
    let v: f64 = s.parse().map_err(|e| format!("not a number: {e}"))?;
    if !(0.0 < v && v < 1.0) {
        return Err(format!("threshold must be in (0,1), got {v}"));
    }
    Ok(v)
}

fn parse_window_override(s: &str) -> Result<(String, u64), String> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| format!("expected MODEL=N, got {s}"))?;
    let n: u64 = v.parse().map_err(|e| format!("bad number {v}: {e}"))?;
    Ok((k.to_string(), n))
}

#[derive(Deserialize)]
struct TranscriptUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

#[derive(Deserialize)]
struct TranscriptMessage {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<TranscriptUsage>,
}

#[derive(Deserialize)]
struct TranscriptRecord {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    message: Option<TranscriptMessage>,
}

struct PeerState {
    cwd: PathBuf,
    /// Discovered via temporal correlation. None until a `kind:"message"` from
    /// this peer has been correlated to a jsonl in their cwd.
    transcript_path: Option<PathBuf>,
    /// Timestamp of the most recent `kind:"message"` from this peer.
    last_message_ts: Option<chrono::DateTime<chrono::Utc>>,
    /// Timestamp of the message ts that produced the current `transcript_path`.
    /// When `last_message_ts != last_correlated_ts`, the next poll re-attempts
    /// correlation. Once it succeeds, the two equalize and the mapping is sticky
    /// until a fresh message arrives. Prevents re-correlation from ping-ponging
    /// transcript_path across other sessions' jsonls in the same cwd.
    last_correlated_ts: Option<chrono::DateTime<chrono::Utc>>,
    /// Indices into the (sorted ascending) thresholds list that have already
    /// been alerted on. Cleared when input drops below the lowest threshold
    /// (compaction observed).
    crossed: HashSet<usize>,
}

struct ChannelState {
    tailer: Tailer,
    peers: HashMap<String, PeerState>,
}

struct Reading {
    model: String,
    input: u64,
}

/// Encode an absolute cwd path the way Claude Code does for project dirs:
/// every '/' becomes '-'.
fn encoded_project_dir(cwd: &Path) -> Option<PathBuf> {
    let s = cwd.to_str()?;
    let encoded: String = s.chars().map(|c| if c == '/' { '-' } else { c }).collect();
    let home = dirs::home_dir()?;
    Some(home.join(".claude").join("projects").join(encoded))
}

/// Asymmetric window for matching a jsonl's mtime against a switchboard
/// message's ts. Backward (jsonl touched before message) is small — only
/// absorbs scheduling jitter. Forward (jsonl touched after message) is large
/// because Claude Code commits a turn's transcript line only after every
/// tool call in that turn completes; a turn with many tool calls can make
/// jsonl mtime lag the switchboard message ts by minutes.
const TS_MATCH_WINDOW_BACK_SECS: u64 = 10;
const TS_MATCH_WINDOW_FWD_SECS: u64 = 600;

/// Find the .jsonl in `project_dir` whose mtime is closest to `ts` within a
/// small window. Used for temporal correlation: when handle H sends a switchboard
/// message at time T, the jsonl whose mtime is within ±10s of T is H's transcript
/// (the session that just took a turn). Returns `None` if no jsonl matches —
/// e.g. the message is old backlog and the file has been touched many times since.
fn jsonl_for_ts(project_dir: &Path, ts: chrono::DateTime<chrono::Utc>) -> Option<PathBuf> {
    let secs = ts.timestamp();
    let nanos = ts.timestamp_subsec_nanos();
    if secs < 0 {
        return None;
    }
    let target =
        std::time::UNIX_EPOCH.checked_add(Duration::new(secs as u64, nanos))?;
    let earliest = target
        .checked_sub(Duration::from_secs(TS_MATCH_WINDOW_BACK_SECS))
        .unwrap_or(std::time::UNIX_EPOCH);
    let latest = target.checked_add(Duration::from_secs(TS_MATCH_WINDOW_FWD_SECS))?;

    let mut best: Option<(PathBuf, SystemTime, Duration)> = None;
    let entries = std::fs::read_dir(project_dir).ok()?;
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let m = match e.metadata().ok().and_then(|m| m.modified().ok()) {
            Some(t) => t,
            None => continue,
        };
        if m < earliest || m > latest {
            continue;
        }
        // Pick the one whose mtime is closest to ts.
        let dist = if m >= target {
            m.duration_since(target).unwrap_or(Duration::ZERO)
        } else {
            target.duration_since(m).unwrap_or(Duration::ZERO)
        };
        if best.as_ref().map(|(_, _, bd)| dist < *bd).unwrap_or(true) {
            best = Some((path, m, dist));
        }
    }
    best.map(|(p, _, _)| p)
}

/// Read the latest assistant turn's model + input footprint from a transcript.
fn read_reading(transcript: &Path) -> Option<Reading> {
    let content = std::fs::read_to_string(transcript).ok()?;
    let mut last: Option<(String, TranscriptUsage)> = None;
    for line in content.lines() {
        if line.is_empty() {
            continue;
        }
        let r: TranscriptRecord = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if r.kind != "assistant" {
            continue;
        }
        if let Some(m) = r.message {
            if let (Some(model), Some(usage)) = (m.model, m.usage) {
                last = Some((model, usage));
            }
        }
    }
    last.map(|(model, u)| Reading {
        model,
        input: u.input_tokens + u.cache_creation_input_tokens + u.cache_read_input_tokens,
    })
}

fn fmt_kilo(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

fn format_warning(handle: &str, model: &str, input: u64, window: u64, pct: f64, action: &str) -> String {
    format!(
        "\u{26a0} {} ({}) at {}/{} ({:.0}%) \u{2014} {}",
        handle,
        model,
        fmt_kilo(input),
        fmt_kilo(window),
        pct * 100.0,
        action
    )
}

fn build_custom_rungs(ratios: &[f64]) -> Vec<ThresholdRung> {
    ratios
        .iter()
        .enumerate()
        .map(|(i, &r)| {
            let idx = i.min(LEVEL_PROGRESSION.len() - 1);
            ThresholdRung {
                ratio: r,
                level: LEVEL_PROGRESSION[idx].0,
                action: LEVEL_PROGRESSION[idx].1,
            }
        })
        .collect()
}

fn bot_join(env: &Env, channel: &str, handle: &str) -> Result<()> {
    if !peer_exists(env, channel, handle) {
        let cwd = std::env::current_dir()
            .ok()
            .and_then(|p| p.to_str().map(str::to_string));
        append_record(env, channel, &Record::join(channel, handle, cwd))?;
    }
    touch_peer(env, channel, handle)?;
    Ok(())
}

/// True when `peers/<handle>` exists and was touched within PEER_STALE_SECS.
/// Filesystem presence + recency is the source of truth for "currently around" —
/// channel join/leave events alone leak ghosts when sessions exit without
/// calling `switchboard leave`.
fn peer_is_fresh(env: &Env, channel: &str, handle: &str) -> bool {
    let path = env.peer_file(channel, handle);
    let Ok(meta) = std::fs::metadata(&path) else {
        return false;
    };
    let Ok(mtime) = meta.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(mtime)
        .map(|d| d <= Duration::from_secs(PEER_STALE_SECS))
        .unwrap_or(false)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Run(args) => run_bot(args),
        Cmd::Start(args) => start_daemon(args),
        Cmd::Stop => stop_daemon(),
    }
}

fn start_daemon(args: RunArgs) -> Result<()> {
    let plist = daemon::plist_path()?;

    if plist.exists() {
        bail!(
            "daemon already installed at {}; run `stop` first",
            plist.display()
        );
    }

    let binary = std::env::current_exe()?.canonicalize()?;
    let run_args = args.to_cli_args();

    let mut env_vars = vec![];
    if let Ok(dir) = std::env::var("SWITCHBOARD_DIR") {
        env_vars.push(("SWITCHBOARD_DIR".into(), dir));
    }

    let content = daemon::generate_plist(&binary, &run_args, &env_vars);

    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&plist, &content)?;
    eprintln!("plist written to {}", plist.display());

    if let Err(e) = daemon::load(&plist) {
        let _ = std::fs::remove_file(&plist);
        bail!("failed to load daemon: {e}");
    }

    eprintln!("daemon started (label: {})", daemon::LABEL);
    Ok(())
}

fn stop_daemon() -> Result<()> {
    let plist = daemon::plist_path()?;

    if !plist.exists() {
        eprintln!("no daemon installed (checked {})", plist.display());
        return Ok(());
    }

    if daemon::is_loaded() {
        daemon::unload(&plist)?;
        eprintln!("daemon unloaded");
    }

    std::fs::remove_file(&plist)?;
    eprintln!("plist removed");
    Ok(())
}

fn run_bot(args: RunArgs) -> Result<()> {
    let custom_rungs: Option<Vec<ThresholdRung>> = if args.threshold.is_empty() {
        None
    } else {
        let mut sorted = args.threshold.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        Some(build_custom_rungs(&sorted))
    };

    let context_overrides: HashMap<String, u64> = args.context_window.into_iter().collect();
    let verbose = args.verbose;
    let handle = args.handle.clone();
    let env = Env::resolve(Some(handle.clone()), None)?;

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    }).expect("failed to set signal handler");

    let initial_channels: Vec<String> = if args.all {
        env.list_channels()?
    } else if !args.channel.is_empty() {
        args.channel.clone()
    } else {
        bail!("must specify --channel <name> (repeatable) or --all");
    };

    if initial_channels.is_empty() {
        bail!("no channels to watch");
    }

    let mut channels: HashMap<String, ChannelState> = HashMap::new();
    for ch in &initial_channels {
        let tailer = Tailer::at_start(&env, ch)?;
        bot_join(&env, ch, &handle)?;
        channels.insert(
            ch.clone(),
            ChannelState {
                tailer,
                peers: HashMap::new(),
            },
        );
    }

    let poll = Duration::from_secs(args.poll);
    let sitrep_interval = if args.sitrep == 0 {
        None
    } else {
        Some(Duration::from_secs(args.sitrep))
    };

    let mut last_poll = Instant::now()
        .checked_sub(poll)
        .unwrap_or_else(Instant::now);
    let mut last_sitrep = Instant::now();

    let threshold_desc = match &custom_rungs {
        Some(cr) => format!("custom {:?}", cr.iter().map(|r| r.ratio).collect::<Vec<_>>()),
        None => "auto (multi=[0.25,0.40,0.50] single=[0.50,0.60,0.70])".to_string(),
    };
    eprintln!(
        "token-watcher started. handle={} channels={:?} thresholds={} poll={}s sitrep={}s",
        handle, initial_channels, threshold_desc, args.poll, args.sitrep
    );

    while running.load(Ordering::SeqCst) {
        // Discover new channels under --all.
        if args.all {
            for ch in env.list_channels()? {
                if !channels.contains_key(&ch) {
                    let tailer = Tailer::at_start(&env, &ch)?;
                    bot_join(&env, &ch, &handle)?;
                    channels.insert(
                        ch.clone(),
                        ChannelState {
                            tailer,
                            peers: HashMap::new(),
                        },
                    );
                }
            }
        }

        // Drain channel events:
        // - "join" → register handle + cwd (no transcript_path yet)
        // - "leave" → drop handle
        // - "message" → temporal correlation: pick the jsonl in this peer's
        //   cwd whose mtime is freshest *right now* (the session that just
        //   took a turn). Sticky cache; only updated when a new message lands.
        for state in channels.values_mut() {
            let tailer = &mut state.tailer;
            let peers = &mut state.peers;
            let bot_handle = handle.as_str();
            let _rotated = tailer.drain(|rec| {
                match rec.kind.as_str() {
                    "join" => {
                        if let (Some(h), Some(cwd)) = (rec.handle.as_ref(), rec.cwd.as_ref()) {
                            if h == bot_handle {
                                return Ok(());
                            }
                            // Don't reset crossed if this handle is already known with the
                            // same cwd — preserves alert-suppression across re-joins.
                            let new_cwd = PathBuf::from(cwd);
                            match peers.get_mut(h) {
                                Some(existing) if existing.cwd == new_cwd => {}
                                _ => {
                                    peers.insert(
                                        h.clone(),
                                        PeerState {
                                            cwd: new_cwd,
                                            transcript_path: None,
                                            last_message_ts: None,
                                            last_correlated_ts: None,
                                            crossed: HashSet::new(),
                                        },
                                    );
                                }
                            }
                        }
                    }
                    "leave" => {
                        if let Some(h) = rec.handle.as_ref() {
                            peers.remove(h);
                        }
                    }
                    "message" => {
                        if let Some(from) = rec.from.as_ref() {
                            if from == bot_handle {
                                return Ok(());
                            }
                            if let Some(ps) = peers.get_mut(from) {
                                ps.last_message_ts = Some(rec.ts);
                                if let Some(project_dir) = encoded_project_dir(&ps.cwd) {
                                    if let Some(jsonl) = jsonl_for_ts(&project_dir, rec.ts) {
                                        if verbose {
                                            eprintln!("verbose: {} → {}", from, jsonl.display());
                                        }
                                        ps.transcript_path = Some(jsonl);
                                        ps.last_correlated_ts = Some(rec.ts);
                                    } else if verbose {
                                        eprintln!("verbose: {}: no matching jsonl in {}", from, project_dir.display());
                                    }
                                } else if verbose {
                                    eprintln!("verbose: {}: no project dir for cwd {}", from, ps.cwd.display());
                                }
                            }
                        }
                    }
                    _ => {}
                }
                Ok(())
            })?;
        }

        // Periodic threshold check.
        if last_poll.elapsed() >= poll {
            for (ch_name, state) in channels.iter_mut() {
                let fresh_peers: usize = state
                    .peers
                    .keys()
                    .filter(|h| peer_is_fresh(&env, ch_name, h))
                    .count();
                let rungs: &[ThresholdRung] = match &custom_rungs {
                    Some(cr) => cr,
                    None if fresh_peers >= 2 => MULTI_AGENT_DEFAULTS,
                    None => SINGLE_AGENT_DEFAULTS,
                };

                let mut warnings: Vec<(String, &'static str)> = vec![];
                for (peer_handle, ps) in state.peers.iter_mut() {
                    if !peer_is_fresh(&env, ch_name, peer_handle) {
                        continue;
                    }
                    if ps.last_message_ts != ps.last_correlated_ts {
                        if let Some(ts) = ps.last_message_ts {
                            if let Some(project_dir) = encoded_project_dir(&ps.cwd) {
                                if let Some(jsonl) = jsonl_for_ts(&project_dir, ts) {
                                    if verbose {
                                        eprintln!("verbose: {} → {} (retry)", peer_handle, jsonl.display());
                                    }
                                    ps.transcript_path = Some(jsonl);
                                    ps.last_correlated_ts = Some(ts);
                                } else if verbose {
                                    eprintln!("verbose: {}: retry failed, no matching jsonl", peer_handle);
                                }
                            }
                        }
                    }
                    let Some(transcript) = ps.transcript_path.as_ref() else {
                        continue;
                    };
                    let reading = match read_reading(transcript) {
                        Some(r) => r,
                        None => continue,
                    };
                    let window = context_overrides
                        .get(&reading.model)
                        .copied()
                        .unwrap_or_else(|| default_context_window(&reading.model));
                    if window == 0 {
                        continue;
                    }

                    let lowest_bound = (rungs[0].ratio * window as f64) as u64;
                    if reading.input < lowest_bound {
                        ps.crossed.clear();
                    }

                    let mut new_idx: Option<usize> = None;
                    for (i, rung) in rungs.iter().enumerate() {
                        let bound = (rung.ratio * window as f64) as u64;
                        if reading.input >= bound && !ps.crossed.contains(&i) {
                            new_idx = Some(i);
                        }
                    }

                    if let Some(i) = new_idx {
                        let pct = reading.input as f64 / window as f64;
                        warnings.push((
                            format_warning(peer_handle, &reading.model, reading.input, window, pct, rungs[i].action),
                            rungs[i].level,
                        ));
                        for j in 0..=i {
                            ps.crossed.insert(j);
                        }
                    }
                }
                for (body, level) in warnings {
                    let rec = Record::service_announcement(ch_name, &handle, level, body);
                    if let Err(e) = append_record(&env, ch_name, &rec) {
                        eprintln!("token-watcher: failed to append warning: {e}");
                    }
                    let _ = touch_peer(&env, ch_name, &handle);
                }
            }
            last_poll = Instant::now();
        }

        // Periodic sitrep (kind:message) summarizing tracked peers per channel.
        if let Some(interval) = sitrep_interval {
            if last_sitrep.elapsed() >= interval {
                for (ch_name, state) in channels.iter() {
                    if state.peers.is_empty() {
                        continue;
                    }
                    let mut entries: Vec<String> = vec![];
                    for (peer_handle, ps) in state.peers.iter() {
                        if !peer_is_fresh(&env, ch_name, peer_handle) {
                            continue;
                        }
                        let Some(transcript) = ps.transcript_path.as_ref() else {
                            entries.push(format!("{peer_handle} (unmapped)"));
                            continue;
                        };
                        match read_reading(transcript) {
                            Some(r) => {
                                let window = context_overrides
                                    .get(&r.model)
                                    .copied()
                                    .unwrap_or_else(|| default_context_window(&r.model));
                                let pct = if window == 0 {
                                    0.0
                                } else {
                                    r.input as f64 / window as f64
                                };
                                entries.push(format!(
                                    "{} {:.0}% ({})",
                                    peer_handle,
                                    pct * 100.0,
                                    r.model
                                ));
                            }
                            None => entries.push(format!("{peer_handle} (no transcript)")),
                        }
                    }
                    if !entries.is_empty() {
                        let body = format!("sitrep: {}", entries.join(", "));
                        let rec = Record::message(ch_name, &handle, vec![], body);
                        if let Err(e) = append_record(&env, ch_name, &rec) {
                            eprintln!("token-watcher: failed to append sitrep: {e}");
                        }
                        let _ = touch_peer(&env, ch_name, &handle);
                    }
                }
                last_sitrep = Instant::now();
            }
        }

        std::thread::sleep(DRAIN_INTERVAL);
    }

    for ch_name in channels.keys() {
        let rec = Record::leave(ch_name, &handle);
        if let Err(e) = append_record(&env, ch_name, &rec) {
            eprintln!("token-watcher: leave failed for {ch_name}: {e}");
        }
        let _ = remove_peer(&env, ch_name, &handle);
    }
    eprintln!("token-watcher: shutdown complete");
    Ok(())
}
