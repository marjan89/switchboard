use anyhow::{Result, bail};
use clap::Parser;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use switchboard::io::{append_record, peer_exists, touch_peer};
use switchboard::paths::Env;
use switchboard::record::Record;
use switchboard::stream::Tailer;

const DEFAULT_HANDLE: &str = "bot-token-watcher";
const DEFAULT_THRESHOLDS: &[f64] = &[0.8, 0.9, 0.95];
const DEFAULT_POLL_SECS: u64 = 5;
const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;
const DRAIN_INTERVAL: Duration = Duration::from_millis(150);

#[derive(Parser)]
#[command(
    name = "switchboard-token-watcher",
    version,
    about = "Switchboard bot: warn participants approaching their model's context-window limit"
)]
struct Cli {
    /// Channels to watch. Repeat for multiple. Mutually exclusive with --all.
    #[arg(long)]
    channel: Vec<String>,

    /// Watch every channel, including future ones. Mutually exclusive with --channel.
    #[arg(long, conflicts_with = "channel")]
    all: bool,

    /// Threshold ratio (0.0-1.0). Repeat for multiple rungs. Default: 0.8 0.9 0.95.
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
    /// Indices into the (sorted ascending) thresholds list that have already
    /// been alerted on. Cleared when the participant's footprint drops below
    /// the lowest threshold (compaction observed).
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

/// Find the most-recently-modified .jsonl in a project dir.
fn newest_jsonl(project_dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(PathBuf, std::time::SystemTime)> = None;
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
        if best.as_ref().map(|(_, bt)| m > *bt).unwrap_or(true) {
            best = Some((path, m));
        }
    }
    best.map(|(p, _)| p)
}

/// Read latest assistant turn's model + input footprint from a transcript JSONL.
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

fn format_warning(handle: &str, model: &str, input: u64, window: u64, pct: f64) -> String {
    format!(
        "⚠ {} ({}) at {}/{} ({:.0}%) — consider /compact",
        handle,
        model,
        fmt_kilo(input),
        fmt_kilo(window),
        pct * 100.0
    )
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

fn main() -> Result<()> {
    let args = Cli::parse();

    let mut thresholds = if args.threshold.is_empty() {
        DEFAULT_THRESHOLDS.to_vec()
    } else {
        args.threshold.clone()
    };
    thresholds.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let context_overrides: HashMap<String, u64> = args.context_window.into_iter().collect();
    let handle = args.handle.clone();
    let env = Env::resolve(Some(handle.clone()), None)?;

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
        // Open at_start so we replay backlog and learn existing joins.
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

    eprintln!(
        "token-watcher started. handle={} channels={:?} thresholds={:?} poll={}s sitrep={}s",
        handle, initial_channels, thresholds, args.poll, args.sitrep
    );

    loop {
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

        // Drain channel events: register on join (with cwd), unregister on leave.
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
                            peers.insert(
                                h.clone(),
                                PeerState {
                                    cwd: PathBuf::from(cwd),
                                    crossed: HashSet::new(),
                                },
                            );
                        }
                    }
                    "leave" => {
                        if let Some(h) = rec.handle.as_ref() {
                            peers.remove(h);
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
                let mut warnings: Vec<(String, &'static str)> = vec![];
                for (peer_handle, ps) in state.peers.iter_mut() {
                    let project_dir = match encoded_project_dir(&ps.cwd) {
                        Some(p) => p,
                        None => continue,
                    };
                    let transcript = match newest_jsonl(&project_dir) {
                        Some(p) => p,
                        None => continue,
                    };
                    let reading = match read_reading(&transcript) {
                        Some(r) => r,
                        None => continue,
                    };
                    let window = context_overrides
                        .get(&reading.model)
                        .copied()
                        .unwrap_or(DEFAULT_CONTEXT_WINDOW);
                    if window == 0 {
                        continue;
                    }

                    let lowest_bound = (thresholds[0] * window as f64) as u64;
                    if reading.input < lowest_bound {
                        ps.crossed.clear();
                    }

                    let mut new_idx: Option<usize> = None;
                    for (i, &t) in thresholds.iter().enumerate() {
                        let bound = (t * window as f64) as u64;
                        if reading.input >= bound && !ps.crossed.contains(&i) {
                            new_idx = Some(i);
                        }
                    }

                    if let Some(i) = new_idx {
                        let pct = reading.input as f64 / window as f64;
                        let level: &'static str = if thresholds[i] >= 0.9 {
                            "critical"
                        } else {
                            "warning"
                        };
                        warnings.push((
                            format_warning(peer_handle, &reading.model, reading.input, window, pct),
                            level,
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
                        let project_dir = match encoded_project_dir(&ps.cwd) {
                            Some(p) => p,
                            None => continue,
                        };
                        let reading = newest_jsonl(&project_dir).and_then(|p| read_reading(&p));
                        match reading {
                            Some(r) => {
                                let window = context_overrides
                                    .get(&r.model)
                                    .copied()
                                    .unwrap_or(DEFAULT_CONTEXT_WINDOW);
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
}
