//! A caller-owned marker requests cooperative cancellation; the scheduler never removes it.
use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Default)]
pub(crate) struct Cancellation {
    path: Option<PathBuf>,
}

impl Cancellation {
    pub(crate) fn new(path: Option<PathBuf>) -> Self {
        Self { path }
    }

    pub(crate) fn requested(&self) -> Result<bool> {
        match &self.path {
            Some(path) => path
                .try_exists()
                .with_context(|| format!("checking cancellation marker {}", path.display())),
            None => Ok(false),
        }
    }

    pub(crate) fn sleep(&self, normal: Duration) {
        let interval = if self.path.is_some() {
            normal.min(Duration::from_millis(100))
        } else {
            normal
        };
        std::thread::sleep(interval);
    }
}
