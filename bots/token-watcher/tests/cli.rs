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
        let mut all_args = vec!["run"];
        all_args.extend(args);
        std::process::Command::new(watcher_bin())
            .env("SWITCHBOARD_DIR", self.sw_dir.path())
            .env("HOME", self.home.path())
            .args(&all_args)
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
        .find(|l| l["kind"] == "join" && l["handle"] == "bot-token-watcher")
        .expect("expected bot to emit kind:join");
    assert_eq!(bot_join["kind"], "join");
    assert_eq!(bot_join["handle"], "bot-token-watcher");
}

#[test]
fn bot_emits_service_announcement_when_threshold_crossed() {
    ensure_switchboard_built();
    let h = Harness::new();
    h.switchboard("alice").args(["send", "hi"]).assert().success();

    let transcript = h.project_dir.join("alice-session.jsonl");
    h.write_assistant_turn(&transcript, "claude-haiku-4-5", 1, 0, 180_000, 100); // 90%

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
    assert!(body.contains("claude-haiku-4-5"), "body: {body}");
    assert!(body.contains("90%"), "expected 90%%; body: {body}");
    assert_eq!(warning.unwrap()["level"], "critical");
}

#[test]
fn bot_does_not_double_warn_same_threshold() {
    ensure_switchboard_built();
    let h = Harness::new();
    h.switchboard("alice").args(["send", "hi"]).assert().success();

    let transcript = h.project_dir.join("alice-session.jsonl");
    h.write_assistant_turn(&transcript, "claude-haiku-4-5", 1, 0, 165_000, 100); // 82.5%

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
    h.write_assistant_turn(&transcript, "claude-haiku-4-5", 1, 0, 180_000, 100); // 90%

    let mut child = h.spawn_bot(&["--channel", "default", "--poll", "1"]);
    thread::sleep(Duration::from_millis(2200));

    // Simulate compaction: rewrite transcript to a small turn well below lowest threshold.
    fs::write(
        &transcript,
        r#"{"type":"assistant","message":{"model":"claude-haiku-4-5","usage":{"input_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":1000,"output_tokens":50}}}
"#,
    )
    .unwrap();
    thread::sleep(Duration::from_millis(1500));

    // Climb back over.
    fs::write(
        &transcript,
        r#"{"type":"assistant","message":{"model":"claude-haiku-4-5","usage":{"input_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":180000,"output_tokens":100}}}
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
    h.write_assistant_turn(&transcript, "claude-haiku-4-5", 1, 0, 40_000, 100); // 20% — under all thresholds

    let mut child = h.spawn_bot(&["--channel", "default", "--poll", "1"]);
    thread::sleep(Duration::from_millis(800));

    h.switchboard("alice").args(["leave"]).assert().success();
    thread::sleep(Duration::from_millis(300));

    // Push transcript over the threshold — mapping was dropped, no warning expected.
    fs::write(
        &transcript,
        r#"{"type":"assistant","message":{"model":"claude-haiku-4-5","usage":{"input_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":180000,"output_tokens":100}}}
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
fn bot_disambiguates_two_handles_in_same_cwd() {
    ensure_switchboard_built();
    let h = Harness::new();

    let alice_jsonl = h.project_dir.join("alice-session.jsonl");
    let bob_jsonl = h.project_dir.join("bob-session.jsonl");

    // alice's transcript: 90% of 200K window — should warn at 0.8.
    h.write_assistant_turn(&alice_jsonl, "claude-haiku-4-5", 1, 0, 180_000, 100);
    // alice sends → her message correlates to alice_jsonl (only jsonl in dir at this point).
    h.switchboard("alice").args(["send", "alice posting"]).assert().success();
    thread::sleep(Duration::from_millis(50));

    // bob's transcript: 20% — under multi-agent 25% threshold, should NOT warn.
    h.write_assistant_turn(&bob_jsonl, "claude-haiku-4-5", 1, 0, 40_000, 100);
    // bob sends → his message correlates to bob_jsonl (now newest in dir).
    h.switchboard("bob").args(["send", "bob posting"]).assert().success();

    let mut child = h.spawn_bot(&["--channel", "default", "--poll", "1"]);
    thread::sleep(Duration::from_millis(2500));
    let _ = child.kill();
    let _ = child.wait();

    let lines = h.read_log_lines("default");
    let warns: Vec<_> = lines
        .iter()
        .filter(|l| l["kind"] == "service_announcement" && l["from"] == "bot-token-watcher")
        .collect();
    assert_eq!(warns.len(), 1, "expected exactly one warning (alice only); got {}", warns.len());
    let body = warns[0]["body"].as_str().unwrap();
    assert!(body.contains("alice"), "expected alice in warning; got: {body}");
    assert!(!body.contains("bob"), "expected NO bob in warning; got: {body}");
    assert!(body.contains("90%"), "expected 90%%; got: {body}");
    assert_eq!(warns[0]["level"], "critical");
}

#[test]
fn bot_culls_ghost_peer_when_presence_file_removed() {
    ensure_switchboard_built();
    let h = Harness::new();

    // alice joins, sends, then her peer file is wiped (simulating crash without leave).
    let alice_jsonl = h.project_dir.join("alice-session.jsonl");
    h.write_assistant_turn(&alice_jsonl, "claude-haiku-4-5", 1, 0, 100_000, 100);
    h.switchboard("alice").args(["send", "hi"]).assert().success();

    // Bot starts with the (still-active) alice peer file present.
    let mut child = h.spawn_bot(&["--channel", "default", "--poll", "1"]);
    thread::sleep(Duration::from_millis(800));

    // Wipe alice's presence file — ghost.
    let alice_peer = h.sw_dir.path().join("default").join("peers").join("alice");
    fs::remove_file(&alice_peer).unwrap();

    // Push alice's transcript way over threshold. With ghost-cull, bot drops her
    // BEFORE the next poll, so no warning.
    fs::write(
        &alice_jsonl,
        r#"{"type":"assistant","message":{"model":"claude-haiku-4-5","usage":{"input_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":195000,"output_tokens":100}}}
"#,
    )
    .unwrap();

    // Wait long enough for cull (10s) and a subsequent poll to confirm no warning.
    thread::sleep(Duration::from_millis(11500));

    let _ = child.kill();
    let _ = child.wait();

    let lines = h.read_log_lines("default");
    let warns: Vec<_> = lines
        .iter()
        .filter(|l| {
            l["kind"] == "service_announcement"
                && l["from"] == "bot-token-watcher"
                && l["body"].as_str().unwrap_or("").contains("alice")
        })
        .collect();
    // We allow at most ONE warning — the very first poll, before the peer file
    // was removed. Any warning AFTER cull would mean the ghost wasn't dropped.
    assert!(
        warns.len() <= 1,
        "expected at most one warning before cull; got {} (ghost not dropped)",
        warns.len()
    );
}

#[test]
fn bot_emits_sitrep_message_when_enabled() {
    ensure_switchboard_built();
    let h = Harness::new();
    h.switchboard("alice").args(["send", "hi"]).assert().success();

    let transcript = h.project_dir.join("alice-session.jsonl");
    h.write_assistant_turn(&transcript, "claude-haiku-4-5", 1, 0, 100_000, 100); // 50%

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
    assert!(body.contains("claude-haiku-4-5"), "sitrep body: {body}");
}

#[test]
fn opus_uses_1m_context_window() {
    ensure_switchboard_built();
    let h = Harness::new();
    h.switchboard("alice").args(["send", "hi"]).assert().success();

    let transcript = h.project_dir.join("alice-session.jsonl");
    // 850K cached on opus-4-7 → 85% of 1M, should warn at 0.8 threshold.
    // If bot incorrectly used 200K, this would be 425% — obviously wrong.
    h.write_assistant_turn(&transcript, "claude-opus-4-7", 1, 0, 850_000, 100);

    let mut child = h.spawn_bot(&["--channel", "default", "--poll", "1"]);
    thread::sleep(Duration::from_millis(2500));
    let _ = child.kill();
    let _ = child.wait();

    let lines = h.read_log_lines("default");
    let warning = lines
        .iter()
        .find(|l| l["kind"] == "service_announcement" && l["from"] == "bot-token-watcher")
        .expect("expected service_announcement for opus at 85%");
    let body = warning["body"].as_str().unwrap();
    assert!(body.contains("1000.0k"), "expected 1M window; got: {body}");
    assert!(body.contains("85%"), "expected 85%%; got: {body}");
    assert!(body.contains("claude-opus-4-7"), "expected model name; got: {body}");
}

#[test]
fn context_window_override_changes_percentage() {
    ensure_switchboard_built();
    let h = Harness::new();
    h.switchboard("alice").args(["send", "hi"]).assert().success();

    let transcript = h.project_dir.join("alice-session.jsonl");
    // 80K cached on haiku — default 200K window → 40% (no warning at 0.8 threshold).
    // With --context-window claude-haiku-4-5=90000, that's 89% → should warn.
    h.write_assistant_turn(&transcript, "claude-haiku-4-5", 1, 0, 80_000, 100);

    let mut child = h.spawn_bot(&[
        "--channel", "default", "--poll", "1",
        "--context-window", "claude-haiku-4-5=90000",
    ]);
    thread::sleep(Duration::from_millis(2500));
    let _ = child.kill();
    let _ = child.wait();

    let lines = h.read_log_lines("default");
    let warning = lines
        .iter()
        .find(|l| l["kind"] == "service_announcement" && l["from"] == "bot-token-watcher")
        .expect("expected warning with overridden window");
    let body = warning["body"].as_str().unwrap();
    assert!(body.contains("90.0k"), "expected 90K window; got: {body}");
    assert!(body.contains("88%") || body.contains("89%"), "expected ~89%%; got: {body}");
}

#[test]
fn custom_threshold_triggers_at_lower_level() {
    ensure_switchboard_built();
    let h = Harness::new();
    h.switchboard("alice").args(["send", "hi"]).assert().success();

    let transcript = h.project_dir.join("alice-session.jsonl");
    // 60K cached on haiku (200K window) → 30%. Single-agent defaults [0.50, 0.60, 0.70] won't fire.
    // With --threshold 0.25, it should.
    h.write_assistant_turn(&transcript, "claude-haiku-4-5", 1, 0, 60_000, 100);

    let mut child = h.spawn_bot(&[
        "--channel", "default", "--poll", "1",
        "--threshold", "0.25",
    ]);
    thread::sleep(Duration::from_millis(2500));
    let _ = child.kill();
    let _ = child.wait();

    let lines = h.read_log_lines("default");
    let warning = lines
        .iter()
        .find(|l| l["kind"] == "service_announcement" && l["from"] == "bot-token-watcher")
        .expect("expected warning at custom 25% threshold");
    let body = warning["body"].as_str().unwrap();
    assert!(body.contains("alice"), "body: {body}");
    assert!(body.contains("30%"), "expected 30%%; got: {body}");
}

#[test]
fn verbose_logs_correlation_to_stderr() {
    ensure_switchboard_built();
    let h = Harness::new();
    h.switchboard("alice").args(["send", "hi"]).assert().success();

    let transcript = h.project_dir.join("alice-session.jsonl");
    h.write_assistant_turn(&transcript, "claude-haiku-4-5", 1, 0, 100_000, 100);

    let mut child = std::process::Command::new(watcher_bin())
        .env("SWITCHBOARD_DIR", h.sw_dir.path())
        .env("HOME", h.home.path())
        .args(["run", "--channel", "default", "--poll", "1", "--verbose"])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    thread::sleep(Duration::from_millis(2500));
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(stderr.contains("verbose:"), "stderr should contain verbose output: {stderr}");
    assert!(stderr.contains("alice"), "verbose stderr should mention alice: {stderr}");
    assert!(stderr.contains(".jsonl"), "verbose stderr should mention jsonl path: {stderr}");
}

#[test]
fn multi_agent_uses_lower_thresholds() {
    ensure_switchboard_built();
    let h = Harness::new();
    h.switchboard("alice").args(["send", "hi"]).assert().success();
    h.switchboard("bob").args(["send", "hi"]).assert().success();

    let alice_jsonl = h.project_dir.join("alice-session.jsonl");
    let bob_jsonl = h.project_dir.join("bob-session.jsonl");
    // alice at 30% — above multi 0.25 threshold, below single 0.50 threshold.
    h.write_assistant_turn(&alice_jsonl, "claude-haiku-4-5", 1, 0, 60_000, 100);
    h.switchboard("alice").args(["send", "posting"]).assert().success();
    thread::sleep(Duration::from_millis(50));
    // bob at 10% — below all thresholds.
    h.write_assistant_turn(&bob_jsonl, "claude-haiku-4-5", 1, 0, 20_000, 100);
    h.switchboard("bob").args(["send", "posting"]).assert().success();

    let mut child = h.spawn_bot(&["--channel", "default", "--poll", "1"]);
    thread::sleep(Duration::from_millis(2500));
    let _ = child.kill();
    let _ = child.wait();

    let lines = h.read_log_lines("default");
    let warns: Vec<_> = lines
        .iter()
        .filter(|l| l["kind"] == "service_announcement" && l["from"] == "bot-token-watcher")
        .collect();
    assert_eq!(warns.len(), 1, "expected 1 warning (alice at 30% > multi 25%); got {}", warns.len());
    let body = warns[0]["body"].as_str().unwrap();
    assert!(body.contains("alice"), "expected alice; got: {body}");
    assert!(body.contains("30%"), "expected 30%%; got: {body}");
    assert!(body.contains("consider /compact"), "expected warn action; got: {body}");
    assert_eq!(warns[0]["level"], "warning");
}

#[test]
fn graceful_shutdown_emits_leave() {
    ensure_switchboard_built();
    let h = Harness::new();
    h.switchboard("alice").args(["send", "hi"]).assert().success();

    let child = std::process::Command::new(watcher_bin())
        .env("SWITCHBOARD_DIR", h.sw_dir.path())
        .env("HOME", h.home.path())
        .args(["run", "--channel", "default", "--poll", "60"])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    thread::sleep(Duration::from_millis(500));

    // Send SIGINT for graceful shutdown.
    unsafe { libc::kill(child.id() as i32, libc::SIGINT); }
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(stderr.contains("shutdown complete"), "stderr: {stderr}");

    let lines = h.read_log_lines("default");
    let bot_leave = lines
        .iter()
        .find(|l| l["kind"] == "leave" && l["handle"] == "bot-token-watcher")
        .expect("expected bot to emit leave on shutdown");
    assert_eq!(bot_leave["kind"], "leave");
    assert_eq!(bot_leave["handle"], "bot-token-watcher");

    let peer = h.sw_dir.path().join("default").join("peers").join("bot-token-watcher");
    assert!(!peer.exists(), "peer file should be removed on shutdown");
}

#[test]
fn stop_without_start_succeeds() {
    let h = Harness::new();
    let output = std::process::Command::new(watcher_bin())
        .env("HOME", h.home.path())
        .args(["stop"])
        .output()
        .unwrap();
    assert!(output.status.success(), "stop should succeed when no daemon installed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no daemon installed"),
        "stderr should mention no daemon: {stderr}"
    );
}

#[test]
fn start_rejects_double_install() {
    let h = Harness::new();
    let plist_dir = h.home.path().join("Library").join("LaunchAgents");
    fs::create_dir_all(&plist_dir).unwrap();
    fs::write(
        plist_dir.join("com.aperture.switchboard-token-watcher.plist"),
        "existing",
    )
    .unwrap();

    let output = std::process::Command::new(watcher_bin())
        .env("HOME", h.home.path())
        .env("SWITCHBOARD_DIR", h.sw_dir.path())
        .args(["start", "--channel", "default"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "start should fail when plist exists"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already installed"),
        "stderr should mention already installed: {stderr}"
    );
}

#[test]
fn older_opus_uses_200k_not_1m() {
    ensure_switchboard_built();
    let h = Harness::new();
    h.switchboard("alice").args(["send", "hi"]).assert().success();

    let transcript = h.project_dir.join("alice-session.jsonl");
    // 120k on opus-4-5 → 60% of 200k (should warn at single-agent 0.50 threshold).
    // Bug: broad "claude-opus-4" prefix assigns 1M window → 120k/1M = 12% → no warning.
    h.write_assistant_turn(&transcript, "claude-opus-4-5-20250415", 1, 0, 120_000, 100);

    let mut child = h.spawn_bot(&["--channel", "default", "--poll", "1"]);
    thread::sleep(Duration::from_millis(2500));
    let _ = child.kill();
    let _ = child.wait();

    let lines = h.read_log_lines("default");
    let warning = lines
        .iter()
        .find(|l| l["kind"] == "service_announcement" && l["from"] == "bot-token-watcher")
        .expect("expected warning for opus-4-5 at 60% of 200k");
    let body = warning["body"].as_str().unwrap();
    assert!(body.contains("200.0k"), "expected 200k window; got: {body}");
    assert!(body.contains("60%"), "expected 60%%; got: {body}");
}
