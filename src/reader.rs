use std::fs::{self, File, Metadata};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

const BATCH_SIZE: usize = 512;
const FOLLOW_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, PartialEq)]
pub enum ReaderEvent {
    /// Complete lines. If a `Partial` line was sent before, the first line completes it.
    Lines(Vec<String>),
    /// The incomplete last line of a followed file (replaces any previous partial line).
    Partial(String),
    /// Something the user should know about (e.g. the file was truncated).
    Notice(String),
    Eof,
}

pub enum InputSource {
    Stdin,
    File {
        file: File,
        path: PathBuf,
        follow: bool,
    },
}

pub fn spawn_reader(source: InputSource, tx: Sender<ReaderEvent>) {
    thread::spawn(move || match source {
        InputSource::Stdin => read_stream(BufReader::new(io::stdin().lock()), tx),
        InputSource::File { file, follow: false, .. } => read_stream(BufReader::new(file), tx),
        InputSource::File { file, path, follow: true } => follow_file(file, &path, tx),
    });
}

/// Convert raw bytes to a line: strip the trailing `\r?\n`, replace invalid UTF-8.
fn bytes_to_line(mut bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_suffix(b"\n") {
        bytes = rest.strip_suffix(b"\r").unwrap_or(rest);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// Collects lines and sends them in batches.
struct Batcher {
    tx: Sender<ReaderEvent>,
    batch: Vec<String>,
}

impl Batcher {
    fn new(tx: Sender<ReaderEvent>) -> Self {
        Self {
            tx,
            batch: Vec::with_capacity(BATCH_SIZE),
        }
    }

    /// Returns false when the receiver is gone.
    fn send(&self, event: ReaderEvent) -> bool {
        self.tx.send(event).is_ok()
    }

    fn flush(&mut self) -> bool {
        if self.batch.is_empty() {
            return true;
        }
        let batch = std::mem::take(&mut self.batch);
        self.send(ReaderEvent::Lines(batch))
    }

    /// Add a line, flushing when the batch is full or no more input is buffered.
    fn push<R>(&mut self, bytes: &[u8], reader: &BufReader<R>) -> bool {
        self.batch.push(bytes_to_line(bytes));
        if self.batch.len() >= BATCH_SIZE || reader.buffer().is_empty() {
            self.flush()
        } else {
            true
        }
    }
}

/// Read until EOF (stdin, or a file that isn't followed).
fn read_stream<R: Read>(mut reader: BufReader<R>, tx: Sender<ReaderEvent>) {
    let mut out = Batcher::new(tx);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            // A final line without a trailing newline is still a line
            Ok(0) => break,
            Ok(_) => {
                if !out.push(&buf, &reader) {
                    return;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                out.flush();
                out.send(ReaderEvent::Notice(format!("Read error: {e}")));
                break;
            }
        }
    }
    if out.flush() {
        out.send(ReaderEvent::Eof);
    }
}

#[cfg(unix)]
fn file_id(meta: &Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_id(_meta: &Metadata) -> Option<(u64, u64)> {
    None
}

/// Follow a file like `tail -F`: keep reading appended data, start over when the file
/// is truncated, and reopen it when it is replaced (log rotation).
fn follow_file(file: File, path: &Path, tx: Sender<ReaderEvent>) {
    let mut out = Batcher::new(tx);
    let mut id = file.metadata().ok().as_ref().and_then(file_id);
    let mut reader = BufReader::new(file);
    let mut pos: u64 = 0;
    // Bytes of the current (possibly incomplete) line
    let mut buf = Vec::new();
    // Length of `buf` when it was last sent as a partial line
    let mut partial_sent = 0;

    loop {
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => {
                if !out.flush() {
                    return;
                }
                if buf.len() > partial_sent {
                    if !out.send(ReaderEvent::Partial(bytes_to_line(&buf))) {
                        return;
                    }
                    partial_sent = buf.len();
                }
                thread::sleep(FOLLOW_POLL_INTERVAL);

                // A missing path is normal for a moment during rotation
                let Ok(meta) = fs::metadata(path) else { continue };
                let replaced = file_id(&meta) != id;
                let truncated = !replaced && meta.len() < pos;
                if !replaced && !truncated {
                    continue;
                }
                if replaced {
                    // Finish whatever was appended to the old file before switching
                    let old_len = reader.get_ref().metadata().map_or(0, |m| m.len());
                    if old_len > pos {
                        continue;
                    }
                    let Ok(new_file) = File::open(path) else { continue };
                    id = new_file.metadata().ok().as_ref().and_then(file_id);
                    reader = BufReader::new(new_file);
                } else if reader.seek(SeekFrom::Start(0)).is_err() {
                    continue;
                }
                pos = 0;

                // The incomplete line really was the last line of the old content
                if !buf.is_empty() {
                    out.batch.push(bytes_to_line(&buf));
                    buf.clear();
                    partial_sent = 0;
                }
                let notice = if replaced {
                    "File was replaced (rotated); following the new file"
                } else {
                    "File was truncated; reading from the start"
                };
                if !out.flush() || !out.send(ReaderEvent::Notice(notice.to_string())) {
                    return;
                }
            }
            Ok(n) => {
                pos += n as u64;
                if buf.ends_with(b"\n") {
                    let ok = out.push(&buf, &reader);
                    buf.clear();
                    partial_sent = 0;
                    if !ok {
                        return;
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                out.flush();
                out.send(ReaderEvent::Notice(format!("Read error: {e}")));
                out.send(ReaderEvent::Eof);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Instant;

    fn collect(rx: &Receiver<ReaderEvent>) -> (Vec<String>, bool) {
        let mut all_lines = Vec::new();
        let mut got_eof = false;
        while let Ok(event) = rx.try_recv() {
            match event {
                ReaderEvent::Lines(batch) => all_lines.extend(batch),
                ReaderEvent::Eof => got_eof = true,
                _ => {}
            }
        }
        (all_lines, got_eof)
    }

    #[test]
    fn test_read_stream_batches_and_eof() {
        let (tx, rx) = channel();
        let data = "line 1\nline 2\r\nline 3\nno newline";
        read_stream(BufReader::new(Cursor::new(data.as_bytes())), tx);

        let (all_lines, got_eof) = collect(&rx);
        assert_eq!(all_lines, vec!["line 1", "line 2", "line 3", "no newline"]);
        assert!(got_eof);
    }

    #[test]
    fn test_invalid_utf8_does_not_stop_reading() {
        let (tx, rx) = channel();
        let data: &[u8] = b"ok1\n\xff\xfe bad\nok2\n";
        read_stream(BufReader::new(Cursor::new(data)), tx);

        let (all_lines, got_eof) = collect(&rx);
        assert_eq!(all_lines, vec!["ok1", "\u{FFFD}\u{FFFD} bad", "ok2"]);
        assert!(got_eof);
    }

    /// Wait for the next event that isn't a (possibly repeated) partial line update.
    fn next_event(rx: &Receiver<ReaderEvent>, skip_partial: bool) -> ReaderEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let ev = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("reader event");
            if !(skip_partial && matches!(ev, ReaderEvent::Partial(_))) {
                return ev;
            }
        }
    }

    #[test]
    fn test_follow_partial_lines_truncation_and_rotation() {
        let dir = std::env::temp_dir().join(format!("track-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.log");
        let append = |s: &str| {
            let mut f = fs::OpenOptions::new().append(true).create(true).open(&path).unwrap();
            f.write_all(s.as_bytes()).unwrap();
        };

        append("one\npart");
        let (tx, rx) = channel();
        spawn_reader(
            InputSource::File { file: File::open(&path).unwrap(), path: path.clone(), follow: true },
            tx,
        );
        assert_eq!(next_event(&rx, false), ReaderEvent::Lines(vec!["one".into()]));
        assert_eq!(next_event(&rx, false), ReaderEvent::Partial("part".into()));

        // Completing the partial line sends it as a complete line
        append("ial\n");
        assert_eq!(next_event(&rx, true), ReaderEvent::Lines(vec!["partial".into()]));

        // Truncation: start over from the beginning
        fs::write(&path, "").unwrap();
        assert!(matches!(next_event(&rx, true), ReaderEvent::Notice(n) if n.contains("truncated")));
        append("after truncate\n");
        assert_eq!(next_event(&rx, true), ReaderEvent::Lines(vec!["after truncate".into()]));

        // Rotation: the old file is renamed and a new one created at the same path
        fs::rename(&path, dir.join("app.log.1")).unwrap();
        append("rotated\n");
        assert!(matches!(next_event(&rx, true), ReaderEvent::Notice(n) if n.contains("rotated")));
        assert_eq!(next_event(&rx, true), ReaderEvent::Lines(vec!["rotated".into()]));

        let _ = fs::remove_dir_all(&dir);
    }
}
