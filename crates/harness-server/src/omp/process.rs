use std::collections::VecDeque;
use std::io::{self, BufRead, Read, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use crate::omp::protocol::FrameDecoder;
use crate::{HarnessServerError, Result};

const STDERR_TAIL_BYTES: usize = 16 * 1024;
const EVENT_QUEUE_DEPTH: usize = 256;

#[derive(Debug)]
pub(crate) enum ProcessEvent {
    Frame(serde_json::Value),
    StdoutError(HarnessServerError),
    Eof,
}

/// A bounded, line-framed OMP RPC process.
///
/// The owner is the only writer. Stdout is read on a dedicated thread into a
/// bounded channel, so a noisy child cannot allocate an unbounded event queue.
/// Physical frame bounds are enforced while reading, before a complete line is
/// allocated.
pub(crate) struct OmpProcess {
    child: Child,
    stdin: ChildStdin,
    events: mpsc::Receiver<ProcessEvent>,
    stderr_tail: Arc<Mutex<VecDeque<u8>>>,
}

impl OmpProcess {
    pub(crate) fn spawn(mut command: Command, max_frame_bytes: usize) -> Result<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let cwd = command
            .get_current_dir()
            .map(ToOwned::to_owned)
            .unwrap_or_default();
        let mut child = command
            .spawn()
            .map_err(|source| HarnessServerError::SpawnHarness { cwd, source })?;
        let stdin = child
            .stdin
            .take()
            .ok_or(HarnessServerError::HarnessStdinUnavailable)?;
        let stdout = child
            .stdout
            .take()
            .ok_or(HarnessServerError::HarnessStdoutUnavailable)?;
        let stderr = child
            .stderr
            .take()
            .ok_or(HarnessServerError::HarnessStderrUnavailable)?;

        let (event_tx, events) = mpsc::sync_channel(EVENT_QUEUE_DEPTH);
        thread::spawn(move || {
            let mut reader = io::BufReader::new(stdout);
            let mut decoder = FrameDecoder::default();
            loop {
                let event = match read_bounded_line(&mut reader, max_frame_bytes) {
                    Ok(Some(line)) => match decoder.push(&line) {
                        Ok(Some(frame)) => ProcessEvent::Frame(frame),
                        Ok(None) => continue,
                        Err(error) => ProcessEvent::StdoutError(error),
                    },
                    Ok(None) => match decoder.finish() {
                        Ok(()) => ProcessEvent::Eof,
                        Err(error) => ProcessEvent::StdoutError(error),
                    },
                    Err(error) => ProcessEvent::StdoutError(error.into()),
                };
                let terminal = !matches!(event, ProcessEvent::Frame(_));
                if event_tx.send(event).is_err() || terminal {
                    break;
                }
            }
        });

        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_BYTES)));
        let stderr_tail_reader = Arc::clone(&stderr_tail);
        thread::spawn(move || {
            let mut stderr = stderr;
            let mut chunk = [0_u8; 1024];
            while let Ok(read) = stderr.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                let mut tail = stderr_tail_reader
                    .lock()
                    .expect("stderr tail lock poisoned");
                for byte in &chunk[..read] {
                    if tail.len() == STDERR_TAIL_BYTES {
                        tail.pop_front();
                    }
                    tail.push_back(*byte);
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            events,
            stderr_tail,
        })
    }

    pub(crate) fn write_json(&mut self, value: &serde_json::Value) -> Result<()> {
        serde_json::to_writer(&mut self.stdin, value)?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }

    pub(crate) fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> std::result::Result<ProcessEvent, mpsc::RecvTimeoutError> {
        self.events.recv_timeout(timeout)
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub(crate) fn kill_and_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match self.child.try_wait()? {
            Some(status) => Ok(Some(status)),
            None => {
                self.child.kill()?;
                self.child.wait().map(Some)
            }
        }
    }

    pub(crate) fn redacted_stderr_tail(&self) -> String {
        let bytes = self
            .stderr_tail
            .lock()
            .expect("stderr tail lock poisoned")
            .len();
        if bytes == 0 {
            String::new()
        } else {
            format!("; stderr tail redacted ({bytes} bytes captured)")
        }
    }
}

impl Drop for OmpProcess {
    fn drop(&mut self) {
        let _ = self.kill_and_wait();
    }
}

fn read_bounded_line<R: BufRead>(reader: &mut R, max_bytes: usize) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index + 1);
        if line.len().saturating_add(take) > max_bytes.saturating_add(1) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("OMP frame exceeds {max_bytes} bytes"),
            ));
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline.is_some() {
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::read_bounded_line;

    #[test]
    fn bounded_reader_rejects_oversized_line_before_returning_it() {
        let mut reader = Cursor::new(b"12345\n".to_vec());
        let error = read_bounded_line(&mut reader, 4).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn bounded_reader_accepts_last_line_without_newline() {
        let mut reader = Cursor::new(b"last".to_vec());
        assert_eq!(
            read_bounded_line(&mut reader, 4).unwrap(),
            Some(b"last".to_vec())
        );
        assert_eq!(read_bounded_line(&mut reader, 4).unwrap(), None);
    }
}
