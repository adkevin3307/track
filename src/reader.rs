use std::fs::{self, File, Metadata};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::Duration;

const BATCH_SIZE: usize = 512;
/// Read buffer for stdin / non-followed input.
const STREAM_BUFFER: usize = 64 * 1024;
/// Longest incomplete line that is shown before it completes (avoids resending huge lines).
const MAX_PARTIAL: usize = 64 * 1024;
const FOLLOW_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, PartialEq)]
pub enum ReaderEvent {
    /// Complete lines. If a `Partial` line was sent before, the first line completes it.
    Lines(Vec<String>),
    /// The incomplete last line so far (replaces any previous partial line).
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

pub fn spawn_reader(source: InputSource, tx: SyncSender<ReaderEvent>) {
    thread::spawn(move || match source {
        // A pipe or device opened by path (e.g. `track <(cmd)`) can't be truncated or
        // rotated: read it like stdin, to its end.
        InputSource::File { file, .. } if is_stream(&file) => {
            read_stream(BufReader::with_capacity(STREAM_BUFFER, file), tx)
        }
        InputSource::Stdin => read_stream(
            BufReader::with_capacity(STREAM_BUFFER, io::stdin().lock()),
            tx,
        ),
        InputSource::File {
            file,
            follow: false,
            ..
        } => read_stream(BufReader::with_capacity(STREAM_BUFFER, file), tx),
        InputSource::File {
            file,
            path,
            follow: true,
        } => follow_file(file, &path, tx),
    });
}

/// A FIFO, socket or character device rather than a regular file.
#[cfg(unix)]
fn is_stream(file: &File) -> bool {
    use std::os::unix::fs::FileTypeExt;
    file.metadata().is_ok_and(|m| {
        let t = m.file_type();
        t.is_fifo() || t.is_socket() || t.is_char_device()
    })
}

#[cfg(not(unix))]
fn is_stream(_file: &File) -> bool {
    false
}

/// Drops the overwritten part of an incomplete line. A carriage return (progress bars,
/// spinners) goes back to the start of the line, so only the text after the last one can
/// still be shown (see [`bytes_to_line`]); without this, a progress line would keep every
/// update in memory. A trailing `\r` is kept: it may be the first half of `\r\n`.
/// Returns true when something was dropped.
fn collapse_cr(pending: &mut Vec<u8>) -> bool {
    let end = pending.len() - usize::from(pending.ends_with(b"\r"));
    match pending[..end].iter().rposition(|&b| b == b'\r') {
        Some(i) => {
            pending.drain(..=i);
            true
        }
        None => false,
    }
}

/// Convert raw bytes to a line: strip the trailing `\r?\n`, replace invalid UTF-8.
///
/// A carriage return inside the line (progress bars, spinners) returns to the start of
/// the line on a terminal, so only the text after the last one is kept.
fn bytes_to_line(mut bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_suffix(b"\n") {
        bytes = rest.strip_suffix(b"\r").unwrap_or(rest);
    }
    if bytes.contains(&b'\r') {
        bytes = bytes
            .rsplit(|&b| b == b'\r')
            .find(|seg| !seg.is_empty())
            .unwrap_or_default();
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/// Collects lines and sends them in batches.
struct Batcher {
    tx: SyncSender<ReaderEvent>,
    batch: Vec<String>,
}

impl Batcher {
    fn new(tx: SyncSender<ReaderEvent>) -> Self {
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
///
/// Lines are sent as soon as the writer pauses, and an incomplete last line is sent as a
/// `Partial` so prompts and progress output show up before their newline arrives.
fn read_stream<R: Read>(mut reader: BufReader<R>, tx: SyncSender<ReaderEvent>) {
    let mut out = Batcher::new(tx);
    // Bytes of the current incomplete line
    let mut pending = Vec::new();
    // Length of `pending` when it was last sent as a partial line
    let mut partial_sent = 0;
    loop {
        let chunk = match reader.fill_buf() {
            Ok([]) => break,
            Ok(chunk) => chunk,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                if !pending.is_empty() {
                    out.batch.push(bytes_to_line(&pending));
                    pending.clear();
                }
                out.flush();
                out.send(ReaderEvent::Notice(format!("Read error: {e}")));
                break;
            }
        };
        let len = chunk.len();
        let mut start = 0;
        while let Some(i) = chunk[start..].iter().position(|&b| b == b'\n') {
            let end = start + i + 1;
            if pending.is_empty() {
                out.batch.push(bytes_to_line(&chunk[start..end]));
            } else {
                pending.extend_from_slice(&chunk[start..end]);
                out.batch.push(bytes_to_line(&pending));
                pending.clear();
            }
            partial_sent = 0;
            start = end;
            if out.batch.len() >= BATCH_SIZE && !out.flush() {
                return;
            }
        }
        pending.extend_from_slice(&chunk[start..]);
        reader.consume(len);
        if collapse_cr(&mut pending) {
            partial_sent = 0;
        }

        // Caught up with the writer: deliver everything read so far
        if !out.flush() {
            return;
        }
        if pending.len() > partial_sent && pending.len() <= MAX_PARTIAL {
            if !out.send(ReaderEvent::Partial(bytes_to_line(&pending))) {
                return;
            }
            partial_sent = pending.len();
        }
    }
    // A final line without a trailing newline is still a line
    if !pending.is_empty() {
        out.batch.push(bytes_to_line(&pending));
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
fn follow_file(file: File, path: &Path, tx: SyncSender<ReaderEvent>) {
    let mut out = Batcher::new(tx);
    let mut id = file.metadata().ok().as_ref().and_then(file_id);
    // Size at the last check: a file is only truncated if it really was bigger. Files in
    // /proc and /sys always report size 0.
    let mut last_len = file.metadata().map_or(0, |m| m.len());
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
                if collapse_cr(&mut buf) {
                    partial_sent = 0;
                }
                if buf.len() > partial_sent {
                    if !out.send(ReaderEvent::Partial(bytes_to_line(&buf))) {
                        return;
                    }
                    partial_sent = buf.len();
                }
                thread::sleep(FOLLOW_POLL_INTERVAL);

                // A missing path is normal for a moment during rotation
                let Ok(meta) = fs::metadata(path) else {
                    continue;
                };
                let replaced = file_id(&meta) != id;
                let truncated = !replaced && meta.len() < pos && last_len >= pos;
                last_len = meta.len();
                if !replaced && !truncated {
                    continue;
                }
                if replaced {
                    // Finish whatever was appended to the old file before switching
                    let old_len = reader.get_ref().metadata().map_or(0, |m| m.len());
                    if old_len > pos {
                        continue;
                    }
                    let Ok(new_file) = File::open(path) else {
                        continue;
                    };
                    id = new_file.metadata().ok().as_ref().and_then(file_id);
                    last_len = new_file.metadata().map_or(0, |m| m.len());
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
    use std::sync::mpsc::{Receiver, sync_channel};
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
        let (tx, rx) = sync_channel(1 << 16);
        let data = "line 1\nline 2\r\nline 3\nno newline";
        read_stream(BufReader::new(Cursor::new(data.as_bytes())), tx);

        let (all_lines, got_eof) = collect(&rx);
        assert_eq!(all_lines, vec!["line 1", "line 2", "line 3", "no newline"]);
        assert!(got_eof);
    }

    #[test]
    fn test_carriage_returns_keep_the_last_text() {
        assert_eq!(
            bytes_to_line(b"\rprogress 10%\rprogress 100%\n"),
            "progress 100%"
        );
        assert_eq!(bytes_to_line(b"done\r\r\n"), "done");
        assert_eq!(bytes_to_line(b"windows line\r\n"), "windows line");
    }

    #[test]
    fn test_stream_sends_complete_lines_before_an_incomplete_one() {
        // The writer prints a line and a prompt, then pauses without a newline
        let (r, mut w) = std::io::pipe().unwrap();
        let (tx, rx) = sync_channel(1 << 16);
        thread::spawn(move || read_stream(BufReader::new(r), tx));
        w.write_all(b"line before\nwaiting...").unwrap();

        assert_eq!(
            next_event(&rx, false),
            ReaderEvent::Lines(vec!["line before".into()])
        );
        assert_eq!(
            next_event(&rx, false),
            ReaderEvent::Partial("waiting...".into())
        );

        w.write_all(b" done\nlast").unwrap();
        drop(w);
        assert_eq!(
            next_event(&rx, true),
            ReaderEvent::Lines(vec!["waiting... done".into()])
        );
        assert_eq!(
            next_event(&rx, true),
            ReaderEvent::Lines(vec!["last".into()])
        );
        assert_eq!(next_event(&rx, true), ReaderEvent::Eof);
    }

    #[test]
    fn test_invalid_utf8_does_not_stop_reading() {
        let (tx, rx) = sync_channel(1 << 16);
        let data: &[u8] = b"ok1\n\xff\xfe bad\nok2\n";
        read_stream(BufReader::new(Cursor::new(data)), tx);

        let (all_lines, got_eof) = collect(&rx);
        assert_eq!(all_lines, vec!["ok1", "\u{FFFD}\u{FFFD} bad", "ok2"]);
        assert!(got_eof);
    }

    /// Wait for the next event that isn't a (possibly repeated) partial line update.
    #[test]
    fn test_collapse_cr_keeps_only_the_visible_text() {
        let mut v = b"10%\r20%\r".to_vec();
        assert!(collapse_cr(&mut v));
        assert_eq!(v, b"20%\r");
        let mut v = b"abc\r".to_vec();
        assert!(!collapse_cr(&mut v));
        let mut v = b"a\rb".to_vec();
        assert!(collapse_cr(&mut v));
        assert_eq!(v, b"b");
    }

    #[test]
    fn test_progress_line_keeps_updating() {
        // 20,000 `\r` updates: far more than the partial-line limit if all were kept.
        let mut data = Vec::new();
        for i in 1..=20_000 {
            write!(data, "\rdownloading... {i} / 20000").unwrap();
        }
        let (tx, rx) = sync_channel(1 << 16);
        read_stream(BufReader::with_capacity(4096, Cursor::new(data)), tx);
        let mut last_partial = None;
        let mut lines = Vec::new();
        while let Ok(event) = rx.try_recv() {
            match event {
                ReaderEvent::Partial(p) => last_partial = Some(p),
                ReaderEvent::Lines(batch) => lines.extend(batch),
                _ => {}
            }
        }
        assert_eq!(
            last_partial.as_deref(),
            Some("downloading... 20000 / 20000")
        );
        assert_eq!(lines, vec!["downloading... 20000 / 20000"]);
    }

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
            let mut f = fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&path)
                .unwrap();
            f.write_all(s.as_bytes()).unwrap();
        };

        append("one\npart");
        let (tx, rx) = sync_channel(1 << 16);
        spawn_reader(
            InputSource::File {
                file: File::open(&path).unwrap(),
                path: path.clone(),
                follow: true,
            },
            tx,
        );
        assert_eq!(
            next_event(&rx, false),
            ReaderEvent::Lines(vec!["one".into()])
        );
        assert_eq!(next_event(&rx, false), ReaderEvent::Partial("part".into()));

        // Completing the partial line sends it as a complete line
        append("ial\n");
        assert_eq!(
            next_event(&rx, true),
            ReaderEvent::Lines(vec!["partial".into()])
        );

        // Truncation: start over from the beginning
        fs::write(&path, "").unwrap();
        assert!(matches!(next_event(&rx, true), ReaderEvent::Notice(n) if n.contains("truncated")));
        append("after truncate\n");
        assert_eq!(
            next_event(&rx, true),
            ReaderEvent::Lines(vec!["after truncate".into()])
        );

        // Rotation: the old file is renamed and a new one created at the same path
        fs::rename(&path, dir.join("app.log.1")).unwrap();
        append("rotated\n");
        assert!(matches!(next_event(&rx, true), ReaderEvent::Notice(n) if n.contains("rotated")));
        assert_eq!(
            next_event(&rx, true),
            ReaderEvent::Lines(vec!["rotated".into()])
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
