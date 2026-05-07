use assert_cmd::Command;
use predicates;
use serde_json::Value;
use std::fs;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

fn cmd(dir: &TempDir, handle: &str) -> Command {
    let mut c = Command::cargo_bin("switchboard").unwrap();
    c.env("SWITCHBOARD_DIR", dir.path());
    c.env("SWITCHBOARD_NAME", handle);
    c.env_remove("SWITCHBOARD_CHANNEL");
    c
}

fn raw_cmd(dir: &TempDir, handle: &str) -> std::process::Command {
    let mut c = std::process::Command::new(assert_cmd::cargo::cargo_bin("switchboard"));
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

#[test]
fn send_reads_stdin_when_no_body_arg() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice")
        .args(["send"])
        .write_stdin("hello from stdin")
        .assert()
        .success();
    let lines = read_log_lines(&dir, "default");
    let msg = lines.iter().find(|l| l["kind"] == "message").unwrap();
    assert_eq!(msg["body"], "hello from stdin");
}

#[test]
fn log_last_returns_only_n_records() {
    let dir = TempDir::new().unwrap();
    for i in 0..5 {
        cmd(&dir, "alice")
            .args(["send", &format!("msg{i}")])
            .assert()
            .success();
    }
    let out = cmd(&dir, "alice")
        .args(["log", "--kind", "message", "--last", "2"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<Value> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["body"], "msg3");
    assert_eq!(lines[1]["body"], "msg4");
}

#[test]
fn status_shows_channel_and_peer_count() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice").args(["send", "hi"]).assert().success();
    cmd(&dir, "bob").args(["send", "hi"]).assert().success();

    let out = cmd(&dir, "alice").args(["status"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("handle:  alice"), "stdout: {stdout}");
    assert!(stdout.contains("channel: default"), "stdout: {stdout}");
    assert!(stdout.contains("2 active"), "stdout: {stdout}");
}

#[test]
fn prune_is_idempotent() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice").args(["send", "hi"]).assert().success();
    let alice_peer = dir.path().join("default").join("peers").join("alice");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
    let f = fs::OpenOptions::new().write(true).open(&alice_peer).unwrap();
    f.set_modified(old).unwrap();
    drop(f);

    cmd(&dir, "admin").args(["prune"]).assert().success();
    assert!(!alice_peer.exists());
    // Second prune on empty peers — should succeed without error.
    cmd(&dir, "admin").args(["prune"]).assert().success();
}

#[test]
fn stream_exclude_self_suppresses_own_messages() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice").args(["send", "from alice"]).assert().success();
    cmd(&dir, "bob").args(["send", "from bob"]).assert().success();

    let mut child = raw_cmd(&dir, "alice")
        .args(["stream", "--from-start", "--exclude-self"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    thread::sleep(Duration::from_millis(500));
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    let lines: Vec<Value> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    let messages: Vec<_> = lines.iter().filter(|l| l["kind"] == "message").collect();
    assert!(
        messages.iter().any(|m| m["body"] == "from bob"),
        "bob's message should be present"
    );
    assert!(
        messages.iter().all(|m| m["from"].as_str() != Some("alice")),
        "alice's messages should be excluded"
    );
}

#[test]
fn stream_kind_filters_backlog() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice").args(["send", "hello"]).assert().success();

    let mut child = raw_cmd(&dir, "bob")
        .args(["stream", "--from-start", "--kind", "message"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    thread::sleep(Duration::from_millis(500));
    let _ = child.kill();
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    let lines: Vec<Value> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    // roster and ready are stream-machinery markers, not log records — they pass through.
    let from_log: Vec<_> = lines
        .iter()
        .filter(|l| l["kind"] != "roster" && l["kind"] != "ready")
        .collect();

    assert!(!from_log.is_empty(), "should have at least one log record");
    for rec in &from_log {
        assert_eq!(
            rec["kind"], "message",
            "expected only message records from log; got {:?}",
            rec
        );
    }
}

#[test]
fn hold_emits_hold_record() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice").args(["hold"]).assert().success();
    let lines = read_log_lines(&dir, "default");
    let hold = lines.iter().find(|l| l["kind"] == "hold").unwrap();
    assert_eq!(hold["from"], "alice");
    let body = hold["body"].as_str().unwrap();
    assert!(body.starts_with("Hold"), "body: {body}");
    assert!(body.contains("wait for resume"), "body: {body}");
}

#[test]
fn resume_emits_resume_record() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice").args(["resume"]).assert().success();
    let lines = read_log_lines(&dir, "default");
    let resume = lines.iter().find(|l| l["kind"] == "resume").unwrap();
    assert_eq!(resume["from"], "alice");
    let body = resume["body"].as_str().unwrap();
    assert!(body.starts_with("Resume"), "body: {body}");
    assert!(body.contains("wire is live"), "body: {body}");
}

#[test]
fn hold_does_not_block_send() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice").args(["hold"]).assert().success();
    cmd(&dir, "bob").args(["send", "still works"]).assert().success();
    let lines = read_log_lines(&dir, "default");
    let msg = lines
        .iter()
        .find(|l| l["kind"] == "message")
        .expect("send should still work during hold");
    assert_eq!(msg["body"], "still works");
    assert_eq!(msg["from"], "bob");
}

#[test]
fn recv_on_empty_channel_returns_nothing() {
    let dir = TempDir::new().unwrap();
    let out = cmd(&dir, "alice").args(["recv"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.trim().is_empty(), "recv on empty channel should return nothing");
}

#[test]
fn log_from_filter_matches_handle_on_join_leave() {
    let dir = TempDir::new().unwrap();
    cmd(&dir, "alice").args(["send", "hi"]).assert().success();
    cmd(&dir, "bob").args(["send", "yo"]).assert().success();
    cmd(&dir, "alice").args(["leave"]).assert().success();

    let out = cmd(&dir, "alice")
        .args(["log", "--from", "alice"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<Value> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 3, "alice: join + message + leave");
    let kinds: Vec<&str> = lines.iter().map(|l| l["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["join", "message", "leave"]);

    let out = cmd(&dir, "bob")
        .args(["log", "--from", "bob"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<Value> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "bob: join + message");
}

#[test]
fn auto_chunk_does_not_split_multibyte_char() {
    let dir = TempDir::new().unwrap();
    let mut body = "x".repeat(4074);
    body.push_str("\u{1F525}"); // 🔥 (4 bytes at position 4074)
    body.push_str(&"y".repeat(100));

    cmd(&dir, "alice")
        .args(["send", &body])
        .assert()
        .success();

    let lines = read_log_lines(&dir, "default");
    let msgs: Vec<_> = lines.iter().filter(|l| l["kind"] == "message").collect();
    assert_eq!(msgs.len(), 2, "expected 2 chunks");

    let total: String = msgs
        .iter()
        .map(|m| {
            let b = m["body"].as_str().unwrap();
            let end = b.find(") ").unwrap();
            b[end + 2..].to_string()
        })
        .collect();

    assert!(total.contains('\u{1F525}'), "emoji should be intact after chunking");
    assert!(!total.contains('\u{FFFD}'), "no replacement characters");
    assert_eq!(total.len(), body.len(), "reassembled length should match original");
}
