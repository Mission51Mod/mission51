//! Versioned JSONL for process consumers. Human diagnostics remain separate.
use anyhow::Result;
use serde_json::{Value, json};
use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub(crate) struct Events {
    enabled: bool,
    run_id: String,
    sequence: u64,
    start: Instant,
}

impl Events {
    pub(crate) fn new(enabled: bool) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self {
            enabled,
            run_id: format!(
                "{}-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_micros(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ),
            sequence: 0,
            start: Instant::now(),
        }
    }

    pub(crate) fn emit(&mut self, event: &str, data: Value) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        self.sequence += 1;
        let record = json!({
            "schema_version": 1,
            "sequence": self.sequence,
            "run_id": self.run_id,
            "elapsed_ms": self.start.elapsed().as_millis(),
            "event": event,
            "data": data,
        });
        let mut output = io::stdout().lock();
        serde_json::to_writer(&mut output, &record)?;
        output.write_all(b"\n")?;
        output.flush()?;
        Ok(())
    }

    pub(crate) fn finish(&mut self, exit_code: i32, mut summary: Value) -> Result<()> {
        let fields = summary.as_object_mut().expect("run summaries are objects");
        fields.insert("exit_code".into(), json!(exit_code));
        fields.insert(
            "elapsed_seconds".into(),
            json!(self.start.elapsed().as_secs_f64()),
        );
        fields.insert(
            "result".into(),
            json!(match exit_code {
                0 => "succeeded",
                130 => "cancelled",
                _ => "failed",
            }),
        );
        self.emit("run_finished", summary)
    }

    pub(crate) fn human(&self, message: &str) {
        if self.enabled {
            eprintln!("{message}");
        } else {
            println!("{message}");
        }
    }
}
