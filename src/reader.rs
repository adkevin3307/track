use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

#[derive(Debug)]
pub enum ReaderEvent {
    Lines(Vec<String>),
    Eof,
}

pub enum InputSource {
    Stdin,
    FilePath(PathBuf),
}

pub fn spawn_reader(source: InputSource, tx: Sender<ReaderEvent>, follow_file: bool) {
    thread::spawn(move || {
        match source {
            InputSource::Stdin => {
                let stdin = io::stdin();
                let reader = BufReader::new(stdin.lock());
                read_stream(reader, tx, false);
            }
            InputSource::FilePath(path) => {
                match File::open(&path) {
                    Ok(file) => {
                        let reader = BufReader::new(file);
                        read_stream(reader, tx, follow_file);
                    }
                    Err(e) => {
                        let _ = tx.send(ReaderEvent::Lines(vec![format!(
                            "Error opening {}: {}",
                            path.display(),
                            e
                        )]));
                        let _ = tx.send(ReaderEvent::Eof);
                    }
                }
            }
        }
    });
}

fn read_stream<R: Read>(
    mut reader: BufReader<R>,
    tx: Sender<ReaderEvent>,
    follow_file: bool,
) {
    let mut batch = Vec::with_capacity(256);
    let mut line_buf = String::new();

    loop {
        line_buf.clear();
        match reader.read_line(&mut line_buf) {
            Ok(0) => {
                // EOF reached
                if !batch.is_empty()
                    && tx.send(ReaderEvent::Lines(std::mem::take(&mut batch))).is_err()
                {
                    return;
                }

                if follow_file {
                    // Like tail -f, poll for more data if following a file
                    thread::sleep(Duration::from_millis(100));
                    continue;
                } else {
                    let _ = tx.send(ReaderEvent::Eof);
                    break;
                }
            }
            Ok(_) => {
                // Strip trailing \r?\n
                if line_buf.ends_with('\n') {
                    line_buf.pop();
                    if line_buf.ends_with('\r') {
                        line_buf.pop();
                    }
                }

                batch.push(std::mem::take(&mut line_buf));

                // If batch is large enough or reader's internal buffer is currently empty, flush immediately
                if (batch.len() >= 512 || reader.buffer().is_empty())
                    && tx.send(ReaderEvent::Lines(std::mem::take(&mut batch))).is_err()
                {
                    return;
                }
            }
            Err(e) => {
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                if !batch.is_empty() {
                    let _ = tx.send(ReaderEvent::Lines(std::mem::take(&mut batch)));
                }
                let _ = tx.send(ReaderEvent::Eof);
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::mpsc::channel;

    #[test]
    fn test_read_stream_batches_and_eof() {
        let (tx, rx) = channel();
        let data = "line 1\nline 2\r\nline 3\n";
        let cursor = Cursor::new(data.as_bytes());
        let reader = BufReader::new(cursor);

        read_stream(reader, tx, false);

        let mut all_lines = Vec::new();
        let mut got_eof = false;

        while let Ok(event) = rx.try_recv() {
            match event {
                ReaderEvent::Lines(batch) => {
                    all_lines.extend(batch);
                }
                ReaderEvent::Eof => {
                    got_eof = true;
                }
            }
        }

        assert_eq!(all_lines, vec!["line 1", "line 2", "line 3"]);
        assert!(got_eof);
    }
}

