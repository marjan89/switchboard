use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

fn switchboard_bin() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.parent().unwrap().parent().unwrap();
    workspace.join("target").join("debug").join("switchboard")
}

fn watcher_bin() -> PathBuf {
    Path::new(env!("CARGO_BIN_EXE_switchboard-token-watcher")).to_path_buf()
}

fn ensure_switchboard_built() {
    if !switchboard_bin().exists() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let status = std::process::Command::new("cargo")
            .args(["build", "--bin", "switchboard"])
            .current_dir(workspace)
            .status()
            .unwrap();
        assert!(status.success(), "failed to build switchboard");
    }
}

/// Encode an absolute path the way Claude Code does for project dirs.
fn encoded(cwd: &Path) -> String {
    cwd.to_str()
        .unwrap()
        .chars()
        .map(|c| if c == '/' { '-' } else { c })
        .collect()
}

/// Build a fake $HOME with a Claude Code project dir mirroring `cwd`.
struct Harness {
    sw_dir: TempDir,
    home: TempDir,
    _cwd: TempDir,
    /// Canonical cwd path — what current_dir() reports under the cwd-set child.
    /// On macOS, /var is a symlink to /private/var, so canonicalize matters.
    cwd_canon: PathBuf,
    project_dir: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let sw_dir = TempDir::new().unwrap();
        let home = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let cwd_canon = fs::canonicalize(cwd.path()).unwrap();
        let project_dir = home
            .path()
            .join(".claude")
            .join("projects")
            .join(encoded(&cwd_canon));
        fs::create_dir_all(&project_dir).unwrap();
        Self {
            sw_dir,
            home,
            _cwd: cwd,
            cwd_canon,
            project_dir,
        }
    }

    fn switchboard(&self, handle: &str) -> Command {
        let mut c = Command::new(switchboard_bin());
        c.env("SWITCHBOARD_DIR", self.sw_dir.path());
        c.env("SWITCHBOARD_NAME", handle);
        c.env("HOME", self.home.path());
        c.env_remove("SWITCHBOARD_CHANNEL");
        c.current_dir(&self.cwd_canon); // makes the join carry our cwd
        c
    }

    fn write_assistant_turn(
        &self,
        transcript: &Path,
        model: &str,
        fresh: u64,
        created: u64,
        cached: u64,
        output: u64,
    ) {
        use std::io::Write;
        let line = format!(
            r#"{{"type":"assistant","message":{{"model":"{model}","usage":{{"input_tokens":{fresh},"cache_creation_input_tokens":{created},"cache_read_input_tokens":{cached},"output_tokens":{output}}}}}}}
"#
        );
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(transcript)
            .unwrap();
        f.write_all(line.as_bytes()).unwrap();
    }

    fn read_log_lines(&self, channel: &str) -> Vec<Value> {
        let path = self.sw_dir.path().join(channel).join("log");
        let content = fs::read_to_string(path).unwrap_or_default();
        content
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn spawn_bot(&self, args: &[&str]) -> std::process::Child {
        std::process::Command::new(watcher_bin())
            .env("SWITCHBOARD_DIR", self.sw_dir.path())
            .env("HOME", self.home.path())
            .args(args)
            .spawn()
            .unwrap()
    }
}

#[test]
fn join_records_carry_cwd() {
    ensure_switchboard_built();
    let h = Harness::new();
    h.switchboard("alice").args(["send", "hi"]).assert().success();
    let lines = h.read_log_lines("default");
    let join = lines
        .iter()
        .find(|l| l["kind"] == "join" && l["handle"] == "alice")
        .unwrap();
    assert_eq!(join["cwd"], h.cwd_canon.to_str().unwrap());
}

#[test]
fn bot_emits_join_on_startup() {
    ensure_switchboard_built();
    let h = Harness::new();
    h.switchboard("alice").args(["send", "hi"]).assert().success();

    let mut child = h.spawn_bot(&["--channel", "default", "--poll", "60"]);
    thread::sleep(Duration::from_millis(500));
    let _ = child.kill();
    let _ = child.wait();

    let lines = h.read_log_lines("default");
    let bot_join = lines
        .iter()
        .find(|l| l["kind"] == "join" && l["handle"] == "bot-token-watcher");
    assert!(bot_join.is_some(), "expected bot to emit kind:join");
}

#[test]
fn bot_emits_service_announcement_when_threshold_crossed() {
    ensure_switchboard_built();
    let h = Harness::new();
    h.switchboard("alice").args(["send", "hi"]).assert().success();

    let transcript = h.project_dir.join("alice-session.jsonl");
    h.write_assistant_turn(&transcript, "claude-opus-4-7", 1, 0, 180_000, 100); // 90%

    let mut child = h.spawn_bot(&["--channel", "default", "--poll", "1"]);
    thread::sleep(Duration::from_millis(2500));
    let _ = child.kill();
    let _ = child.wait();

    let lines = h.read_log_lines("default");
    let warning = lines
        .iter()
        .find(|l| l["kind"] == "service_announcement" && l["from"] == "bot-token-watcher");
    assert!(warning.is_some(), "expected service_announcement; got {} records", lines.len());
    let body = warning.unwrap()["body"].as_str().unwrap();
    assert!(body.contains("alice"), "body: {body}");
    assert!(body.contains("claude-opus-4-7"), "body: {body}");
    assert!(body.contains("90%") || body.contains("89%"), "body: {body}");
    assert_eq!(warning.unwrap()["level"], "critical");
}

#[test]
fn bot_does_not_double_warn_same_threshold() {
    ensure_switchboard_built();
    let h = Harness::new();
    h.switchboard("alice").args(["send", "hi"]).assert().success();

    let transcript = h.project_dir.join("alice-session.jsonl");
    h.write_assistant_turn(&transcript, "claude-opus-4-7", 1, 0, 165_000, 100); // 82.5%

    let mut child = h.spawn_bot(&["--channel", "default", "--poll", "1"]);
    thread::sleep(Duration::from_millis(3500));
    let _ = child.kill();
    let _ = child.wait();

    let lines = h.read_log_lines("default");
    let warns: Vec<_> = lines
        .iter()
        .filter(|l| l["kind"] == "service_announcement")
        .collect();
    assert_eq!(
        warns.len(),
        1,
        "expected exactly one warning across multiple polls; got {}",
        warns.len()
    );
}

#[test]
fn bot_re_arms_after_compaction() {
    ensure_switchboard_built();
    let h = Harness::new();
    h.switchboard("alice").args(["send", "hi"]).assert().success();

    let transcript = h.project_dir.join("alice-session.jsonl");
    h.write_assistant_turn(&transcript, "claude-opus-4-7", 1, 0, 180_000, 100); // 90%

    let mut child = h.spawn_bot(&["--channel", "default", "--poll", "1"]);
    thread::sleep(Duration::from_millis(2200));

    // Simulate compaction: rewrite transcript to a small turn well below lowest threshold.
    fs::write(
        &transcript,
        r#"{"type":"assistant","message":{"model":"claude-opus-4-7","usage":{"input_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":1000,"output_tokens":50}}}
"#,
    )
    .unwrap();
    thread::sleep(Duration::from_millis(1500));

    // Climb back over.
    fs::write(
        &transcript,
        r#"{"type":"assistant","message":{"model":"claude-opus-4-7","usage":{"input_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":180000,"output_tokens":100}}}
"#,
    )
    .unwrap();
    thread::sleep(Duration::from_millis(2200));

    let _ = child.kill();
    let _ = child.wait();

    let lines = h.read_log_lines("default");
    let warns: Vec<_> = lines
        .iter()
        .filter(|l| l["kind"] == "service_announcement" && l["from"] == "bot-token-watcher")
        .collect();
    assert!(
        warns.len() >= 2,
        "expected re-armed warning after compaction drop; got {} warnings",
        warns.len()
    );
}

#[test]
fn bot_drops_mapping_on_leave() {
    ensure_switchboard_built();
    let h = Harness::new();
    h.switchboard("alice").args(["send", "hi"]).assert().success();

    let transcript = h.project_dir.join("alice-session.jsonl");
    h.write_assistant_turn(&transcript, "claude-opus-4-7", 1, 0, 100_000, 100); // 50% — under

    let mut child = h.spawn_bot(&["--channel", "default", "--poll", "1"]);
    thread::sleep(Duration::from_millis(800));

    h.switchboard("alice").args(["leave"]).assert().success();
    thread::sleep(Duration::from_millis(300));

    // Push transcript over the threshold — mapping was dropped, no warning expected.
    fs::write(
        &transcript,
        r#"{"type":"assistant","message":{"model":"claude-opus-4-7","usage":{"input_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":180000,"output_tokens":100}}}
"#,
    )
    .unwrap();
    thread::sleep(Duration::from_millis(2200));

    let _ = child.kill();
    let _ = child.wait();

    let lines = h.read_log_lines("default");
    let warns: Vec<_> = lines
        .iter()
        .filter(|l| l["kind"] == "service_announcement" && l["from"] == "bot-token-watcher")
        .collect();
    assert_eq!(
        warns.len(),
        0,
        "expected no warnings after leave; got {} warnings",
        warns.len()
    );
}

#[test]
fn bot_emits_sitrep_message_when_enabled() {
    ensure_switchboard_built();
    let h = Harness::new();
    h.switchboard("alice").args(["send", "hi"]).assert().success();

    let transcript = h.project_dir.join("alice-session.jsonl");
    h.write_assistant_turn(&transcript, "claude-opus-4-7", 1, 0, 100_000, 100); // 50%

    let mut child = h.spawn_bot(&["--channel", "default", "--poll", "60", "--sitrep", "1"]);
    thread::sleep(Duration::from_millis(2500));
    let _ = child.kill();
    let _ = child.wait();

    let lines = h.read_log_lines("default");
    let sitreps: Vec<_> = lines
        .iter()
        .filter(|l| {
            l["kind"] == "message"
                && l["from"] == "bot-token-watcher"
                && l["body"].as_str().unwrap_or("").starts_with("sitrep:")
        })
        .collect();
    assert!(!sitreps.is_empty(), "expected at least one sitrep message");
    let body = sitreps[0]["body"].as_str().unwrap();
    assert!(body.contains("alice"), "sitrep body: {body}");
    assert!(body.contains("claude-opus-4-7"), "sitrep body: {body}");
}
