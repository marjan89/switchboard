use assert_cmd::Command;
use predicates;
use serde_json::Value;
use std::fs;
use tempfile::TempDir;

fn cmd(dir: &TempDir, handle: &str) -> Command {
    let mut c = Command::cargo_bin("switchboard").unwrap();
    c.env("SWITCHBOARD_DIR", dir.path());
    c.env("SWITCHBOARD_NAME", handle);
    c.env_remove("SWITCHBOARD_CHANNEL");
    c
}

fn read_log_lines(dir: &TempDir, channel: &str) -> Vec<Value> {
    let path = dir.path().join(channel).join("log");
    let content = fs::read_to_string(path).unwrap_or_default();
    content
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn first_send_auto_emits_join_then_message() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice").args(["send", "hello"]).assert().success();
    let lines = read_log_lines(&dir, "default");
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["kind"], "join");
    assert_eq!(lines[0]["handle"], "alice");
    assert_eq!(lines[1]["kind"], "message");
    assert_eq!(lines[1]["body"], "hello");
}

#[test]
fn second_send_does_not_re_emit_join() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice").args(["send", "one"]).assert().success();
    cmd(&dir, "alice").args(["send", "two"]).assert().success();
    let lines = read_log_lines(&dir, "default");
    let joins: Vec<_> = lines.iter().filter(|l| l["kind"] == "join").collect();
    assert_eq!(joins.len(), 1);
    assert_eq!(lines.len(), 3);
}

#[test]
fn append_only_ordering_preserved() {
    let dir = TempDir::new().unwrap();
    for i in 0..5 {
        cmd(&dir, "alice")
            .args(["send", &format!("msg{i}")])
            .assert()
            .success();
    }
    let lines = read_log_lines(&dir, "default");
    let msgs: Vec<&str> = lines
        .iter()
        .filter(|l| l["kind"] == "message")
        .map(|l| l["body"].as_str().unwrap())
        .collect();
    assert_eq!(msgs, vec!["msg0", "msg1", "msg2", "msg3", "msg4"]);
}

#[test]
fn body_over_4kb_auto_chunks() {
    let dir = TempDir::new().unwrap();
    let big = "x".repeat(10000);
    cmd(&dir, "alice")
        .args(["send", &big])
        .assert()
        .success();
    let lines = read_log_lines(&dir, "default");
    let msgs: Vec<_> = lines.iter().filter(|l| l["kind"] == "message").collect();
    assert!(msgs.len() >= 3, "expected at least 3 chunks; got {}", msgs.len());
    let first_body = msgs[0]["body"].as_str().unwrap();
    assert!(first_body.starts_with("(1/"), "first chunk missing (1/N) marker: {first_body}");
    let total: String = msgs.iter()
        .map(|m| {
            let b = m["body"].as_str().unwrap();
            let end = b.find(") ").unwrap();
            b[end + 2..].to_string()
        })
        .collect();
    assert_eq!(total.len(), 10000, "reassembled chunks should equal original length");
}

#[test]
fn multiline_body_round_trips_in_one_record() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice")
        .args(["send", "line1\nline2\nline3"])
        .assert()
        .success();
    let lines = read_log_lines(&dir, "default");
    let msg = lines.iter().find(|l| l["kind"] == "message").unwrap();
    assert_eq!(msg["body"], "line1\nline2\nline3");
}

#[test]
fn targeted_send_records_to_array() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice")
        .args(["send", "--to", "bob,ops", "ping"])
        .assert()
        .success();
    let lines = read_log_lines(&dir, "default");
    let msg = lines.iter().find(|l| l["kind"] == "message").unwrap();
    let to: Vec<&str> = msg["to"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(to, vec!["bob", "ops"]);
}

#[test]
fn leave_emits_event_and_removes_peer_file() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice").args(["send", "hi"]).assert().success();
    let peer = dir.path().join("default").join("peers").join("alice");
    assert!(peer.exists());
    cmd(&dir, "alice").args(["leave"]).assert().success();
    assert!(!peer.exists());
    let lines = read_log_lines(&dir, "default");
    let leaves: Vec<_> = lines.iter().filter(|l| l["kind"] == "leave").collect();
    assert_eq!(leaves.len(), 1);
    assert_eq!(leaves[0]["handle"], "alice");
}

#[test]
fn mark_read_advances_cursor_to_eof() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice").args(["send", "one"]).assert().success();
    cmd(&dir, "alice").args(["send", "two"]).assert().success();

    cmd(&dir, "bob").args(["mark-read"]).assert().success();
    let cursor = dir.path().join("default").join("cursor.bob");
    let val: u64 = fs::read_to_string(&cursor).unwrap().trim().parse().unwrap();
    let log_len = fs::metadata(dir.path().join("default").join("log")).unwrap().len();
    assert_eq!(val, log_len);
}

#[test]
fn channels_lists_active_only_by_default() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice").args(["--channel", "ch-a", "send", "hi"]).assert().success();
    fs::create_dir_all(dir.path().join("ch-empty").join("peers")).unwrap();

    let out = cmd(&dir, "alice").args(["channels"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"ch\":\"ch-a\""));
    assert!(!stdout.contains("\"ch\":\"ch-empty\""));

    let out = cmd(&dir, "alice").args(["channels", "--all"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"ch\":\"ch-a\""));
    assert!(stdout.contains("\"ch\":\"ch-empty\""));
}

#[test]
fn log_filters_by_kind_and_from() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice").args(["send", "a1"]).assert().success();
    cmd(&dir, "bob").args(["send", "b1"]).assert().success();

    let out = cmd(&dir, "alice").args(["log", "--kind", "message"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2);
    for line in &lines {
        let v: Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["kind"], "message");
    }

    let out = cmd(&dir, "alice").args(["log", "--from", "bob"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "--from bob should match join (handle) + message (from)");
    let kinds: Vec<&str> = lines.iter().map(|l| {
        let v: Value = serde_json::from_str(l).unwrap();
        match v["kind"].as_str().unwrap() {
            "join" => "join",
            "message" => "message",
            other => panic!("unexpected kind: {other}"),
        }
    }).collect();
    assert!(kinds.contains(&"join"), "should include bob's join");
    assert!(kinds.contains(&"message"), "should include bob's message");
}

#[test]
fn handle_with_slash_rejected() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "../../etc/passwd")
        .args(["send", "hi"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("path separators"));
}

#[test]
fn handle_with_dotdot_rejected() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "..")
        .args(["send", "hi"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("must not be '.' or '..'"));
}

#[test]
fn handle_with_control_char_rejected() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "evil\nhandle")
        .args(["send", "hi"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("control characters"));
}

#[test]
fn channel_with_slash_rejected() {
    let dir = TempDir::new().unwrap();
    let mut c = Command::cargo_bin("switchboard").unwrap();
    c.env("SWITCHBOARD_DIR", dir.path());
    c.env("SWITCHBOARD_NAME", "alice");
    c.env("SWITCHBOARD_CHANNEL", "../../tmp/pwned");
    c.args(["send", "hi"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("path separators"));
}

#[test]
fn phantom_leave_is_noop() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice").args(["send", "hi"]).assert().success();
    cmd(&dir, "ghost").args(["leave"]).assert().success();
    let lines = read_log_lines(&dir, "default");
    let leaves: Vec<_> = lines.iter().filter(|l| l["kind"] == "leave").collect();
    assert_eq!(leaves.len(), 0, "phantom leave should not emit a record");
}

#[test]
fn double_leave_emits_only_once() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice").args(["send", "hi"]).assert().success();
    cmd(&dir, "alice").args(["leave"]).assert().success();
    cmd(&dir, "alice").args(["leave"]).assert().success();
    let lines = read_log_lines(&dir, "default");
    let leaves: Vec<_> = lines.iter().filter(|l| l["kind"] == "leave").collect();
    assert_eq!(leaves.len(), 1, "double leave should emit only one record");
}

#[test]
fn recv_returns_since_cursor_then_advances() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice").args(["send", "one"]).assert().success();
    cmd(&dir, "alice").args(["send", "two"]).assert().success();
    cmd(&dir, "alice").args(["send", "three"]).assert().success();

    let out = cmd(&dir, "bob").args(["recv"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<Value> = stdout.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let msgs: Vec<&str> = lines.iter()
        .filter(|l| l["kind"] == "message")
        .map(|l| l["body"].as_str().unwrap())
        .collect();
    assert_eq!(msgs, vec!["one", "two", "three"]);

    let out = cmd(&dir, "bob").args(["recv"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.trim().is_empty(), "second recv should return nothing");

    cmd(&dir, "alice").args(["send", "four"]).assert().success();
    cmd(&dir, "alice").args(["send", "five"]).assert().success();

    let out = cmd(&dir, "bob").args(["recv"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<Value> = stdout.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let msgs: Vec<&str> = lines.iter()
        .filter(|l| l["kind"] == "message")
        .map(|l| l["body"].as_str().unwrap())
        .collect();
    assert_eq!(msgs, vec!["four", "five"]);
}

#[test]
fn prune_removes_stale_peers() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice").args(["send", "hi"]).assert().success();
    cmd(&dir, "bob").args(["send", "hi"]).assert().success();

    // Backdate alice's peer file to make it stale (> 300s).
    let alice_peer = dir.path().join("default").join("peers").join("alice");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
    let f = fs::OpenOptions::new().write(true).open(&alice_peer).unwrap();
    f.set_modified(old).unwrap();
    drop(f);

    cmd(&dir, "admin").args(["prune"]).assert().success();

    assert!(!alice_peer.exists(), "stale alice should be pruned");
    let bob_peer = dir.path().join("default").join("peers").join("bob");
    assert!(bob_peer.exists(), "fresh bob should survive prune");
}
