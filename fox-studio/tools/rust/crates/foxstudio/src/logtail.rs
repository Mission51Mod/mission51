//! The end of a log file (build.log, logs/<stage>.log), re-read off the UI thread when its size or mtime changes.
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant, SystemTime};

/// how much of the end of a file is shown
pub const TAIL_BYTES: u64 = 512 * 1024;

type FileSignature = (u64, SystemTime);

struct TailRead {
    signature: Option<FileSignature>,
    lines: Vec<String>,
    truncated: bool,
}

pub struct LogTail {
    path: PathBuf,
    sig: Option<FileSignature>,
    pub lines: Vec<String>,
    /// the file is longer than what is shown
    pub truncated: bool,
    pub missing: bool,
    last_check: Option<Instant>,
    pending: Option<Receiver<TailRead>>,
}

impl LogTail {
    pub fn new(path: PathBuf) -> LogTail {
        LogTail {
            path,
            sig: None,
            lines: vec![],
            truncated: false,
            missing: true,
            last_check: None,
            pending: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Check at most every `every`; a changed file is read on a short-lived thread. Call each frame while shown.
    pub fn refresh(&mut self, ctx: &egui::Context, every: Duration) {
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(update) => {
                    self.missing = update.signature.is_none();
                    self.sig = update.signature;
                    self.lines = update.lines;
                    self.truncated = update.truncated;
                    self.pending = None;
                }
                // the reader found the file unchanged and sent nothing
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.pending = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
            }
        }
        if self.last_check.is_some_and(|t| t.elapsed() < every) {
            ctx.request_repaint_after(every);
            return;
        }
        self.last_check = Some(Instant::now());
        ctx.request_repaint_after(every);
        let path = self.path.clone();
        let old = self.sig;
        let (tx, rx) = channel();
        let ctx2 = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("fox-studio: log tail".into())
            .spawn(move || {
                let sig = std::fs::metadata(&path)
                    .ok()
                    .and_then(|m| m.modified().ok().map(|t| (m.len(), t)));
                if sig == old && sig.is_some() {
                    return; // unchanged: drop the sender, nothing to send
                }
                let (lines, truncated) = match sig {
                    Some((len, _)) => read_tail(&path, len),
                    None => (vec![], false),
                };
                let _ = tx.send(TailRead {
                    signature: sig,
                    lines,
                    truncated,
                });
                ctx2.request_repaint();
            });
        if spawned.is_ok() {
            self.pending = Some(rx);
        }
    }

    /// a read is in flight
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }
}

fn read_tail(path: &Path, len: u64) -> (Vec<String>, bool) {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return (vec![], false);
    };
    let start = len.saturating_sub(TAIL_BYTES);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return (vec![], false);
    }
    let mut buf = Vec::with_capacity((len - start) as usize);
    let _ = f.take(TAIL_BYTES).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text
        .lines()
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0); // partial first line
    }
    (lines, start > 0)
}
