// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ninfer_monitor::tail::{Tail, TailMessage, TailStatus, spawn};

const FAST: Duration = Duration::from_millis(50);

fn write_lines(path: &Path, lines: &[&str]) {
    let mut file = File::create(path).unwrap();
    for line in lines {
        file.write_all(format!("{line}\n").as_bytes()).unwrap();
    }
}

fn append_lines(path: &Path, lines: &[&str]) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    for line in lines {
        file.write_all(format!("{line}\n").as_bytes()).unwrap();
    }
}

fn bytes(lines: &[&str]) -> Vec<Vec<u8>> {
    lines.iter().map(|l| l.as_bytes().to_vec()).collect()
}

#[test]
fn backfill_reads_existing_lines_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    write_lines(&path, &["l1", "l2", "l3"]);

    let mut tail = Tail::new(&path, FAST);
    let result = tail.poll();
    assert_eq!(result.status, TailStatus::Live);
    assert_eq!(result.lines, bytes(&["l1", "l2", "l3"]));
    assert!(result.last_event_time.is_some());

    let result = tail.poll();
    assert!(result.lines.is_empty());
    assert_eq!(result.status, TailStatus::Live);
}

#[test]
fn appended_lines_arrive_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    write_lines(&path, &["first"]);

    let mut tail = Tail::new(&path, FAST);
    assert_eq!(tail.poll().lines, bytes(&["first"]));

    append_lines(&path, &["second", "third"]);
    let result = tail.poll();
    assert_eq!(result.lines, bytes(&["second", "third"]));
}

#[test]
fn partial_line_is_held_back_until_completed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    File::create(&path).unwrap().write_all(b"hel").unwrap();

    let mut tail = Tail::new(&path, FAST);
    assert!(tail.poll().lines.is_empty());
    assert!(tail.poll().lines.is_empty());

    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"lo\n")
        .unwrap();
    let result = tail.poll();
    assert_eq!(result.lines, bytes(&["hello"]));
}

#[test]
fn truncation_triggers_reopen_from_start() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    write_lines(&path, &["a", "b", "c", "d"]);

    let mut tail = Tail::new(&path, FAST);
    assert_eq!(tail.poll().lines, bytes(&["a", "b", "c", "d"]));

    File::options()
        .write(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    append_lines(&path, &["x", "y"]);
    let result = tail.poll();
    assert_eq!(result.lines, bytes(&["x", "y"]));
}

#[test]
fn shrink_below_previous_size_triggers_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    write_lines(&path, &["aaaaaaaaaa", "bbbbbbbbbb"]);

    let mut tail = Tail::new(&path, FAST);
    assert_eq!(tail.poll().lines.len(), 2);

    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    file.write_all(b"cc\n").unwrap();
    let result = tail.poll();
    assert_eq!(result.lines, bytes(&["cc"]));
}

#[test]
fn rotation_reopens_new_file_from_start() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    write_lines(&path, &["old1", "old2"]);

    let mut tail = Tail::new(&path, FAST);
    assert_eq!(tail.poll().lines, bytes(&["old1", "old2"]));

    fs::remove_file(&path).unwrap();
    write_lines(&path, &["new1"]);
    let result = tail.poll();
    assert_eq!(result.lines, bytes(&["new1"]));

    append_lines(&path, &["new2"]);
    let result = tail.poll();
    assert_eq!(result.lines, bytes(&["new2"]));
}

#[test]
fn delete_yields_disconnected_then_live_on_recreate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    write_lines(&path, &["old"]);

    let mut tail = Tail::new(&path, FAST);
    assert_eq!(tail.poll().status, TailStatus::Live);

    fs::remove_file(&path).unwrap();
    let result = tail.poll();
    assert_eq!(result.status, TailStatus::Disconnected);
    assert!(result.lines.is_empty());

    write_lines(&path, &["new1", "new2"]);
    let result = tail.poll();
    assert_eq!(result.status, TailStatus::Live);
    assert_eq!(result.lines, bytes(&["new1", "new2"]));
}

#[test]
fn partial_line_is_discarded_when_file_is_recreated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    File::create(&path).unwrap().write_all(b"hel").unwrap();

    let mut tail = Tail::new(&path, FAST);
    assert!(tail.poll().lines.is_empty());

    fs::remove_file(&path).unwrap();
    assert_eq!(tail.poll().status, TailStatus::Disconnected);

    write_lines(&path, &["lo", "world"]);
    let result = tail.poll();
    assert_eq!(result.lines, bytes(&["lo", "world"]));
}

#[test]
fn missing_file_retries_until_created() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");

    let mut tail = Tail::new(&path, FAST);
    assert_eq!(tail.poll().status, TailStatus::Disconnected);

    write_lines(&path, &["a"]);
    let result = tail.poll();
    assert_eq!(result.status, TailStatus::Live);
    assert_eq!(result.lines, bytes(&["a"]));
}

#[test]
fn empty_file_is_live_with_no_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    File::create(&path).unwrap();

    let mut tail = Tail::new(&path, FAST);
    let result = tail.poll();
    assert_eq!(result.status, TailStatus::Live);
    assert!(result.lines.is_empty());
    assert!(result.last_event_time.is_none());
}

#[test]
fn crlf_line_endings_are_stripped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    File::create(&path)
        .unwrap()
        .write_all(b"abc\r\ndef\n")
        .unwrap();

    let mut tail = Tail::new(&path, FAST);
    let result = tail.poll();
    assert_eq!(result.lines, bytes(&["abc", "def"]));
}

#[test]
fn non_utf8_bytes_pass_through() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    File::create(&path)
        .unwrap()
        .write_all(b"\xff\xfe\x00bin\n")
        .unwrap();

    let mut tail = Tail::new(&path, FAST);
    let result = tail.poll();
    assert_eq!(result.lines, vec![b"\xff\xfe\x00bin".to_vec()]);
}

#[test]
fn new_from_end_skips_existing_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    write_lines(&path, &["old1", "old2"]);

    let mut tail = Tail::new_from_end(&path, FAST);
    assert!(tail.poll().lines.is_empty());

    append_lines(&path, &["fresh"]);
    let result = tail.poll();
    assert_eq!(result.lines, bytes(&["fresh"]));
}

#[test]
fn last_event_time_tracks_new_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    write_lines(&path, &["a"]);

    let mut tail = Tail::new(&path, FAST);
    let first = tail.poll();
    assert!(first.last_event_time.is_some());
    let t1 = first.last_event_time.unwrap();

    let second = tail.poll();
    assert_eq!(second.last_event_time, Some(t1));

    append_lines(&path, &["b"]);
    let third = tail.poll();
    assert!(
        third.last_event_time > Some(t1),
        "last_event_time must advance when a new line arrives"
    );
}

#[test]
fn backfill_large_file_is_fast() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    let mut file = File::create(&path).unwrap();
    for i in 0..2600 {
        let line = format!(
            "{{\"event\":\"throughput\",\"i\":{i},\"pad\":\"{}\"}}\n",
            "x".repeat(2300)
        );
        file.write_all(line.as_bytes()).unwrap();
    }
    drop(file);

    let mut tail = Tail::new(&path, FAST);
    let start = Instant::now();
    let result = tail.poll();
    let elapsed = start.elapsed();

    assert_eq!(result.status, TailStatus::Live);
    assert_eq!(result.lines.len(), 2600);
    assert!(
        elapsed < Duration::from_secs(10),
        "backfill took {elapsed:?}"
    );
}

#[test]
fn a_single_ten_megabyte_line_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    let line = format!(
        "{{\"event\":\"throughput\",\"pad\":\"{}\"}}",
        "x".repeat(10_000_000)
    );
    let mut file = File::create(&path).unwrap();
    file.write_all(line.as_bytes()).unwrap();
    file.write_all(b"\n").unwrap();
    drop(file);

    let mut tail = Tail::new(&path, FAST);
    let result = tail.poll();
    assert_eq!(result.status, TailStatus::Live);
    assert_eq!(result.lines.len(), 1);
    assert_eq!(result.lines[0], line.as_bytes());
}

#[test]
fn spawn_delivers_lines_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    write_lines(&path, &["one"]);

    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let handle = spawn(Tail::new(path.clone(), FAST), tx, stop.clone());

    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while got.is_empty() && Instant::now() < deadline {
        match rx.recv_timeout(FAST) {
            Ok(TailMessage::Line(line)) => got.push(line),
            Ok(TailMessage::Status { .. }) => {}
            Err(_) => {}
        }
    }
    assert_eq!(got, bytes(&["one"]));

    append_lines(&path, &["two", "three"]);
    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while got.len() < 2 && Instant::now() < deadline {
        match rx.recv_timeout(FAST) {
            Ok(TailMessage::Line(line)) => got.push(line),
            Ok(TailMessage::Status { .. }) => {}
            Err(_) => {}
        }
    }
    assert_eq!(got, bytes(&["two", "three"]));

    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
}

#[test]
fn spawn_stops_on_flag() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    write_lines(&path, &["a"]);

    let (tx, _rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(true));
    let handle = spawn(Tail::new(path, FAST), tx, stop);
    handle.join().unwrap();
}

#[test]
fn spawn_sends_status_with_last_event_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log.jsonl");
    write_lines(&path, &["a"]);

    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let handle = spawn(Tail::new(path, FAST), tx, stop.clone());

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut status = None;
    while Instant::now() < deadline {
        match rx.recv_timeout(FAST) {
            Ok(TailMessage::Status {
                status: s,
                last_event_time: Some(_),
            }) => {
                status = Some(s);
                break;
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    assert_eq!(status, Some(TailStatus::Live));

    stop.store(true, Ordering::Relaxed);
    handle.join().unwrap();
}
