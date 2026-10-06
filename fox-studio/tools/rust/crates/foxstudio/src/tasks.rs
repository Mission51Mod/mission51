//! Background work. A task runs on its own thread at below-normal priority (its rayon work too), reports log lines,
//! progress and partial results through a channel, and wakes the UI. The UI polls it once per frame: no locks held
//! across frames, nothing ever blocks the UI thread.
use eframe::egui;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

enum Msg<T> {
    Log(String),
    Progress(f32, String),
    Partial(T),
    Done(Result<T, String>),
}

/// Handle given to the work closure.
pub struct TaskCtx<T> {
    tx: Sender<Msg<T>>,
    ctx: egui::Context,
}

// a handle a worker can pass into library callbacks (e.g. foxinstall's progress); no T: Clone needed
impl<T> Clone for TaskCtx<T> {
    fn clone(&self) -> Self {
        TaskCtx { tx: self.tx.clone(), ctx: self.ctx.clone() }
    }
}

impl<T> TaskCtx<T> {
    pub fn log(&self, s: impl Into<String>) {
        let _ = self.tx.send(Msg::Log(s.into()));
        self.ctx.request_repaint();
    }
    /// fraction 0..1 (negative = indeterminate) and a short phase text
    pub fn progress(&self, fraction: f32, phase: impl Into<String>) {
        let _ = self.tx.send(Msg::Progress(fraction, phase.into()));
        self.ctx.request_repaint();
    }
    /// an early, partial result (e.g. the graph before its hashes are checked)
    pub fn partial(&self, value: T) {
        let _ = self.tx.send(Msg::Partial(value));
        self.ctx.request_repaint();
    }
}

pub struct Task<T> {
    pub label: String,
    pub started: Instant,
    pub log: Vec<String>,
    pub progress: f32,
    pub phase: String,
    rx: Receiver<Msg<T>>,
    partial: Option<T>,
    result: Option<Result<T, String>>,
    pub finished: Option<Duration>,
}

impl<T: Send + 'static> Task<T> {
    pub fn spawn(label: impl Into<String>, ctx: &egui::Context, work: impl FnOnce(&TaskCtx<T>) -> Result<T, String> + Send + 'static) -> Task<T> {
        Self::spawn_with_pool(label, ctx, work, true)
    }

    /// One low-priority worker, without a rayon pool (status and other serial I/O).
    pub fn spawn_serial(label: impl Into<String>, ctx: &egui::Context,
                        work: impl FnOnce(&TaskCtx<T>) -> Result<T, String> + Send + 'static) -> Task<T> {
        Self::spawn_with_pool(label, ctx, work, false)
    }

    fn spawn_with_pool(label: impl Into<String>, ctx: &egui::Context,
                       work: impl FnOnce(&TaskCtx<T>) -> Result<T, String> + Send + 'static, use_pool: bool) -> Task<T> {
        let (tx, rx) = channel();
        let tctx = TaskCtx { tx: tx.clone(), ctx: ctx.clone() };
        let label = label.into();
        let thread_label = label.clone();
        let spawned = std::thread::Builder::new().name(format!("fox-studio: {thread_label}")).spawn(move || {
            lower_thread_priority();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if use_pool { run_low_priority(|| work(&tctx)) } else { work(&tctx) }
            }));
            let r = match r {
                Ok(r) => r,
                Err(p) => Err(format!(
                    "internal error: {}",
                    p.downcast_ref::<String>().cloned().or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()
                )),
            };
            let _ = tx.send(Msg::Done(r));
            tctx.ctx.request_repaint();
        });
        let mut t = Task { label, started: Instant::now(), log: vec![], progress: -1.0, phase: String::new(), rx, partial: None,
                           result: None, finished: None };
        if let Err(e) = spawned {
            t.result = Some(Err(format!("could not start a worker thread: {e}")));
            t.finished = Some(Duration::ZERO);
        }
        t
    }
}

impl<T> Task<T> {
    /// drain messages; true once the task has finished
    pub fn poll(&mut self) -> bool {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Log(s) => self.log.push(s),
                Msg::Progress(f, p) => {
                    self.progress = f;
                    self.phase = p;
                }
                Msg::Partial(v) => self.partial = Some(v),
                Msg::Done(r) => {
                    self.result = Some(r);
                    self.finished = Some(self.started.elapsed());
                }
            }
        }
        self.result.is_some()
    }
    pub fn is_running(&self) -> bool {
        self.result.is_none()
    }
    pub fn take_partial(&mut self) -> Option<T> {
        self.partial.take()
    }
    pub fn take_result(&mut self) -> Option<Result<T, String>> {
        self.result.take()
    }
    pub fn elapsed(&self) -> Duration {
        self.finished.unwrap_or_else(|| self.started.elapsed())
    }
}

/// below-normal priority for the calling thread (best effort)
pub fn lower_thread_priority() {
    #[cfg(windows)]
    unsafe {
        unsafe extern "system" {
            fn GetCurrentThread() -> isize;
            fn SetThreadPriority(h: isize, priority: i32) -> i32;
        }
        SetThreadPriority(GetCurrentThread(), -1); // THREAD_PRIORITY_BELOW_NORMAL
    }
}

/// Run `f` inside a rayon pool whose threads are below normal and leave two cores free (parallel library code,
/// e.g. the .mgsv writer's deflate, then stays polite to a running game or build).
fn run_low_priority<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).saturating_sub(2).max(1);
    match rayon::ThreadPoolBuilder::new().num_threads(n).start_handler(|_| lower_thread_priority()).build() {
        Ok(pool) => pool.install(f),
        Err(_) => f(),
    }
}
