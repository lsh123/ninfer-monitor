// NInferMonitor
//
// This is free software; see the LICENSE.md file in the source distribution for precise wording.
//
// Copyright (C) 2026 Aleksey Sanin aleksey@aleksey.com. All Rights Reserved.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);

const HEAD_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TailStatus {
    Live,
    Disconnected,
    Paused,
}

impl std::fmt::Display for TailStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TailStatus::Live => "Live",
            TailStatus::Disconnected => "Disconnected",
            TailStatus::Paused => "Paused",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone)]
pub struct PollResult {
    pub lines: Vec<Vec<u8>>,
    pub status: TailStatus,
    pub last_event_time: Option<SystemTime>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileId(u128);

// Rotation detection uses file identity, size, and the first `HEAD_LEN`
// bytes. On Windows the identity is the creation time: NTFS timestamps have
// 100 ns granularity and the u128 encoding below is lossless, so a
// rotated/recreated file gets a new identity. An in-place rewrite is detected
// when the captured head bytes change; a rewrite that keeps the first
// `HEAD_LEN` bytes identical is still undetectable.
fn file_identity(meta: &fs::Metadata) -> FileId {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        FileId(((meta.dev() as u128) << 64) | meta.ino() as u128)
    }
    #[cfg(windows)]
    {
        let created = meta.created().unwrap_or(SystemTime::UNIX_EPOCH);
        let dur = created
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default();
        FileId(((dur.as_secs() as u128) << 32) | dur.subsec_nanos() as u128)
    }
    #[cfg(not(any(unix, windows)))]
    {
        FileId(0)
    }
}

fn open_shared(path: &Path) -> std::io::Result<File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_SHARE_WRITE: u32 = 0x2;
        const FILE_SHARE_DELETE: u32 = 0x4;
        OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(path)
    }
    #[cfg(not(windows))]
    {
        File::open(path)
    }
}

pub struct Tail {
    path: PathBuf,
    poll_interval: Duration,
    file: Option<File>,
    identity: Option<FileId>,
    offset: u64,
    last_size: Option<u64>,
    line_buf: Vec<u8>,
    head: Vec<u8>,
    last_event_time: Option<SystemTime>,
    paused: bool,
    start_at_end: bool,
}

impl Tail {
    pub fn new(path: impl Into<PathBuf>, poll_interval: Duration) -> Self {
        Self {
            path: path.into(),
            poll_interval,
            file: None,
            identity: None,
            offset: 0,
            last_size: None,
            line_buf: Vec::new(),
            head: Vec::new(),
            last_event_time: None,
            paused: false,
            start_at_end: false,
        }
    }

    pub fn new_from_end(path: impl Into<PathBuf>, poll_interval: Duration) -> Self {
        let mut tail = Self::new(path, poll_interval);
        tail.start_at_end = true;
        tail
    }

    pub fn poll_interval(&self) -> Duration {
        self.poll_interval
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    pub fn poll(&mut self) -> PollResult {
        let mut connected = false;

        match fs::metadata(&self.path) {
            Ok(meta) => {
                let id = file_identity(&meta);
                let size = meta.len();
                let rotated = self.identity.is_some_and(|prev| prev != id);
                let truncated =
                    size < self.offset || self.last_size.is_some_and(|prev| size < prev);
                if rotated || truncated {
                    self.close_file();
                    self.offset = 0;
                }
                if let Some(file) = self.file.take() {
                    let mut file = file;
                    let offset = self.offset;
                    let changed = head_changed(&mut file, &self.head, offset);
                    if changed {
                        self.close_file();
                        self.offset = 0;
                    } else {
                        self.file = Some(file);
                    }
                }
                if self.file.is_none() {
                    let start = if self.start_at_end {
                        size
                    } else if self.identity == Some(id) {
                        self.offset
                    } else {
                        0
                    };
                    match open_shared(&self.path) {
                        Ok(file) => {
                            let mut file = file;
                            let head = capture_head(&mut file);
                            self.start_at_end = false;
                            self.file = Some(file);
                            self.identity = Some(id);
                            self.offset = start;
                            self.last_size = Some(size);
                            self.head = head;
                        }
                        Err(_) => self.file = None,
                    }
                }
                if let Some(mut file) = self.file.take() {
                    if size > self.offset {
                        let mut buf = Vec::new();
                        let ok = file.seek(SeekFrom::Start(self.offset)).is_ok()
                            && file.read_to_end(&mut buf).is_ok_and(|n| {
                                self.offset += n as u64;
                                self.line_buf.extend_from_slice(&buf);
                                true
                            });
                        if ok {
                            self.file = Some(file);
                            connected = true;
                        }
                    } else {
                        self.file = Some(file);
                        connected = true;
                    }
                }
                if self.file.is_some() {
                    self.last_size = Some(size);
                }
            }
            Err(_) => self.close_file(),
        }

        let lines = if connected {
            let lines = split_complete_lines(&mut self.line_buf);
            if !lines.is_empty() {
                self.last_event_time = Some(SystemTime::now());
            }
            lines
        } else {
            Vec::new()
        };

        let status = if self.paused {
            TailStatus::Paused
        } else if connected {
            TailStatus::Live
        } else {
            TailStatus::Disconnected
        };

        PollResult {
            lines,
            status,
            last_event_time: self.last_event_time,
        }
    }

    fn close_file(&mut self) {
        self.file = None;
        self.identity = None;
        self.last_size = None;
        self.line_buf.clear();
        self.head.clear();
    }
}

fn capture_head(file: &mut File) -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = file.seek(SeekFrom::Start(0));
    let _ = file.take(HEAD_LEN as u64).read_to_end(&mut buf);
    buf
}

fn head_changed(file: &mut File, expected: &[u8], offset: u64) -> bool {
    if expected.is_empty() {
        return false;
    }
    let mut buf = vec![0u8; expected.len()];
    let ok = file.seek(SeekFrom::Start(0)).is_ok() && file.read_exact(&mut buf).is_ok();
    let _ = file.seek(SeekFrom::Start(offset));
    if !ok {
        return true;
    }
    buf != expected
}

pub(crate) fn split_complete_lines(buf: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < buf.len() {
        if buf[i] == b'\n' {
            let mut end = i;
            if end > start && buf[end - 1] == b'\r' {
                end -= 1;
            }
            lines.push(buf[start..end].to_vec());
            start = i + 1;
        }
        i += 1;
    }
    if start > 0 {
        buf.drain(..start);
    }
    lines
}

pub enum TailMessage {
    Line(Vec<u8>),
    Status {
        status: TailStatus,
        last_event_time: Option<SystemTime>,
    },
}

pub fn spawn(tail: Tail, tx: mpsc::Sender<TailMessage>, stop: Arc<AtomicBool>) -> JoinHandle<()> {
    thread::Builder::new()
        .name("tail".to_string())
        .spawn(move || {
            let mut tail = tail;
            let mut last_status = None;
            let mut last_time = None;
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let result = tail.poll();
                if (last_status, last_time) != (Some(result.status), result.last_event_time) {
                    let msg = TailMessage::Status {
                        status: result.status,
                        last_event_time: result.last_event_time,
                    };
                    if tx.send(msg).is_err() {
                        break;
                    }
                    last_status = Some(result.status);
                    last_time = result.last_event_time;
                }
                for line in result.lines {
                    if tx.send(TailMessage::Line(line)).is_err() {
                        return;
                    }
                }
                sleep_interruptible(tail.poll_interval(), &stop);
            }
        })
        .expect("failed to spawn tail thread")
}

/// Sleep for `duration`, waking early if `stop` is set (checked every 50 ms)
/// so a long poll interval does not delay shutdown.
fn sleep_interruptible(duration: Duration, stop: &Arc<AtomicBool>) {
    const SLICE: Duration = Duration::from_millis(50);
    let deadline = Instant::now() + duration;
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        thread::sleep((deadline - now).min(SLICE));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn lines_of(lines: &[&[u8]]) -> Vec<Vec<u8>> {
        lines.iter().map(|l| l.to_vec()).collect()
    }

    #[test]
    fn split_extracts_complete_lines_and_keeps_partial() {
        let mut buf = b"abc\ndef\nghi".to_vec();
        let lines = split_complete_lines(&mut buf);
        assert_eq!(lines, lines_of(&[b"abc", b"def"]));
        assert_eq!(buf, b"ghi".to_vec());
    }

    #[test]
    fn split_strips_trailing_cr() {
        let mut buf = b"abc\r\ndef\r\n".to_vec();
        let lines = split_complete_lines(&mut buf);
        assert_eq!(lines, lines_of(&[b"abc", b"def"]));
        assert!(buf.is_empty());
    }

    #[test]
    fn split_empty_buffer_yields_nothing() {
        let mut buf = Vec::new();
        assert!(split_complete_lines(&mut buf).is_empty());
        assert!(buf.is_empty());
    }

    #[test]
    fn split_bare_newline_yields_empty_line() {
        let mut buf = b"\n".to_vec();
        let lines = split_complete_lines(&mut buf);
        assert_eq!(lines, vec![Vec::<u8>::new()]);
        assert!(buf.is_empty());
    }

    #[test]
    fn split_blank_lines_are_preserved() {
        let mut buf = b"a\n\nb\n".to_vec();
        let lines = split_complete_lines(&mut buf);
        assert_eq!(lines, lines_of(&[b"a", b"", b"b"]));
    }

    #[test]
    fn split_without_newline_keeps_everything() {
        let mut buf = b"no newline yet".to_vec();
        assert!(split_complete_lines(&mut buf).is_empty());
        assert_eq!(buf, b"no newline yet".to_vec());
    }

    #[test]
    fn split_inner_cr_is_not_a_line_ending() {
        let mut buf = b"a\rb\n".to_vec();
        let lines = split_complete_lines(&mut buf);
        assert_eq!(lines, lines_of(&[b"a\rb"]));
    }

    #[test]
    fn split_is_incremental_across_calls() {
        let mut buf = b"hel".to_vec();
        assert!(split_complete_lines(&mut buf).is_empty());
        buf.extend_from_slice(b"lo\nwor");
        let lines = split_complete_lines(&mut buf);
        assert_eq!(lines, lines_of(&[b"hello"]));
        buf.extend_from_slice(b"ld\n");
        let lines = split_complete_lines(&mut buf);
        assert_eq!(lines, lines_of(&[b"world"]));
        assert!(buf.is_empty());
    }

    #[test]
    fn split_large_buffer_is_fast() {
        let mut buf = Vec::new();
        for i in 0..2600 {
            buf.extend_from_slice(
                format!(
                    "{{\"event\":\"throughput\",\"i\":{i},\"pad\":\"{}\"}}\n",
                    "x".repeat(2300)
                )
                .as_bytes(),
            );
        }
        let start = Instant::now();
        let lines = split_complete_lines(&mut buf);
        let elapsed = start.elapsed();
        assert_eq!(lines.len(), 2600);
        assert!(buf.is_empty());
        assert!(
            elapsed < Duration::from_millis(500),
            "split took {elapsed:?}"
        );
    }

    #[test]
    fn in_place_rewrite_with_same_size_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.txt");
        let original = format!("{}\n", "a".repeat(79));
        std::fs::write(&path, &original).unwrap();
        let mut tail = Tail::new(&path, Duration::from_millis(10));
        let first = tail.poll();
        assert_eq!(first.lines, vec![original.trim_end().as_bytes().to_vec()]);

        let rewritten = format!("{}\n", "b".repeat(79));
        std::fs::write(&path, &rewritten).unwrap();
        let second = tail.poll();
        assert_eq!(second.lines, vec![rewritten.trim_end().as_bytes().to_vec()]);

        std::fs::write(&path, &rewritten).unwrap();
        let third = tail.poll();
        assert!(third.lines.is_empty());
    }
}
