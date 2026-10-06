//! Bounded JSONLv1 decoding for child-process stdout. Human stderr is separate.
//!
//! Prefer [`BuildEventStream::push_bytes`] for process reads, then call
//! [`BuildEventStream::finish`] after stdout EOF and the actual child wait.
//! A `run_finished` record alone never establishes successful completion.

use serde_json::{Map, Value};
use std::fmt;

/// Maximum wire-line size, including any LF/CRLF delimiter.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_RUN_ID_BYTES: usize = 256;
const MAX_EVENT_BYTES: usize = 128;
const MAX_ERROR_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownEventKind {
    RunStarted,
    StageDeclared,
    Status,
    StagePlanned,
    StageStarted,
    StageFinished,
    Progress,
    Warning,
    Error,
    RunFinished,
}

impl KnownEventKind {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "run_started" => Self::RunStarted,
            "stage_declared" => Self::StageDeclared,
            "status" => Self::Status,
            "stage_planned" => Self::StagePlanned,
            "stage_started" => Self::StageStarted,
            "stage_finished" => Self::StageFinished,
            "progress" => Self::Progress,
            "warning" => Self::Warning,
            "error" => Self::Error,
            "run_finished" => Self::RunFinished,
            _ => return None,
        })
    }
}

/// A validated envelope. Unknown data and envelope fields remain available.
#[derive(Debug, Clone, PartialEq)]
pub struct BuildEvent {
    pub schema_version: u64,
    pub sequence: u64,
    pub run_id: String,
    pub elapsed_ms: u64,
    pub event: String,
    pub kind: Option<KnownEventKind>,
    pub data: Map<String, Value>,
    pub extra: Map<String, Value>,
    pub terminal: Option<TerminalInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunResult {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerminalInfo {
    pub result: RunResult,
    pub exit_code: i32,
    pub elapsed_seconds: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamErrorKind {
    LineTooLong,
    Framing,
    InvalidUtf8,
    MalformedJson,
    InvalidEnvelope,
    UnsupportedSchema,
    MissingSequence,
    DuplicateSequence,
    OutOfOrderSequence,
    WrongRun,
    ElapsedRegression,
    InvalidEventData,
    DuplicateRunStarted,
    DuplicateTerminal,
    EventAfterTerminal,
    TerminalContradiction,
}

/// The first error is sticky and bounded; ignoring it cannot restore success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamError {
    pub kind: StreamErrorKind,
    pub message: String,
}

impl StreamError {
    fn new(kind: StreamErrorKind, message: impl Into<String>) -> Self {
        let mut message = message.into();
        if message.len() > MAX_ERROR_BYTES {
            let mut end = MAX_ERROR_BYTES - 3;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
            message.push_str("...");
        }
        Self { kind, message }
    }
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StreamError {}

/// Observed intervention, not a request to create a cancellation marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interruption {
    CallerCancellation,
    ForcedTermination,
    BrokenOutputTransport,
}

/// Supply this only after the caller drains stdout and reaps the child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildExit {
    pub code: Option<i32>,
    pub interruption: Option<Interruption>,
}

impl ChildExit {
    pub fn exited(code: i32) -> Self {
        Self {
            code: Some(code),
            interruption: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionOutcome {
    Succeeded,
    Failed,
    Cancelled,
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionIssue {
    Protocol(StreamError),
    TruncatedLine { bytes: usize },
    MissingRunStarted,
    MissingTerminal,
    MissingChildExit,
    ExitMismatch { reported: i32, actual: i32 },
    Interrupted(Interruption),
}

#[derive(Debug, Clone, PartialEq)]
pub struct BuildCompletion {
    pub outcome: CompletionOutcome,
    /// True only when the complete protocol and actual exit agree.
    pub confirmed: bool,
    pub run_id: Option<String>,
    pub accepted_events: u64,
    pub terminal: Option<TerminalInfo>,
    pub child_exit: ChildExit,
    /// At most seven small issues, independent of the number of events.
    pub issues: Vec<CompletionIssue>,
}

/// Retains one partial line and small integrity state, never an event history.
#[derive(Debug)]
pub struct BuildEventStream {
    max_line_bytes: usize,
    pending: Vec<u8>,
    run_id: Option<String>,
    sequence: u64,
    elapsed_ms: u64,
    started: bool,
    saw_failure: bool,
    terminal: Option<TerminalInfo>,
    error: Option<StreamError>,
}

impl Default for BuildEventStream {
    fn default() -> Self {
        Self::new()
    }
}

impl BuildEventStream {
    pub fn new() -> Self {
        Self {
            max_line_bytes: MAX_LINE_BYTES,
            pending: Vec::new(),
            run_id: None,
            sequence: 0,
            elapsed_ms: 0,
            started: false,
            saw_failure: false,
            terminal: None,
            error: None,
        }
    }

    pub fn with_max_line_bytes(limit: usize) -> Result<Self, StreamError> {
        if limit == 0 || limit > MAX_LINE_BYTES {
            return Err(StreamError::new(
                StreamErrorKind::LineTooLong,
                format!("Line limit must be between 1 and {MAX_LINE_BYTES} bytes."),
            ));
        }
        Ok(Self {
            max_line_bytes: limit,
            ..Self::new()
        })
    }

    pub fn accepted_events(&self) -> u64 {
        self.sequence
    }

    pub fn buffered_bytes(&self) -> usize {
        self.pending.len()
    }

    /// Decode exactly one caller-framed stdout record. Optional LF/CRLF accepted.
    /// Without a delimiter, the caller is responsible for proving completeness.
    /// Use `push_bytes` when EOF/truncation must be checked from raw reads.
    pub fn push_line(&mut self, line: &str) -> Result<BuildEvent, StreamError> {
        self.check_error()?;
        if !self.pending.is_empty() {
            return self.poison(StreamError::new(
                StreamErrorKind::Framing,
                "Cannot push a framed line while a raw stdout line is buffered.",
            ));
        }
        let decoded = decode_line(line.as_bytes(), self.max_line_bytes);
        self.accept_decoded(decoded)
    }

    /// Decode arbitrary stdout chunks, delivering each record immediately.
    /// Stops at the first error; keep the decoder for `finish` and drain/reap in
    /// the runner. UTF-8 is validated only after a complete LF-delimited line.
    pub fn push_bytes(
        &mut self,
        mut chunk: &[u8],
        mut on_event: impl FnMut(BuildEvent),
    ) -> Result<(), StreamError> {
        self.check_error()?;
        while !chunk.is_empty() {
            let newline = chunk.iter().position(|byte| *byte == b'\n');
            let take = newline.map_or(chunk.len(), |pos| pos + 1);
            if self.pending.len().saturating_add(take) > self.max_line_bytes {
                return self.poison(StreamError::new(
                    StreamErrorKind::LineTooLong,
                    format!(
                        "Stdout JSONL line exceeds {} bytes; increase producer granularity.",
                        self.max_line_bytes
                    ),
                ));
            }
            // Exact reserve avoids unbounded growth beyond the wire-line cap.
            self.pending.reserve_exact(take);
            self.pending.extend_from_slice(&chunk[..take]);
            chunk = &chunk[take..];
            if newline.is_some() {
                let decoded = decode_line(&self.pending, self.max_line_bytes);
                self.pending.clear();
                on_event(self.accept_decoded(decoded)?);
            }
        }
        Ok(())
    }

    /// Reconcile EOF and actual child exit. Consumes the stream exactly once.
    pub fn finish(self, child_exit: ChildExit) -> BuildCompletion {
        let mut issues = Vec::new();
        if let Some(error) = self.error {
            issues.push(CompletionIssue::Protocol(error));
        }
        if !self.pending.is_empty() {
            issues.push(CompletionIssue::TruncatedLine {
                bytes: self.pending.len(),
            });
        }
        if !self.started {
            issues.push(CompletionIssue::MissingRunStarted);
        }
        if self.terminal.is_none() {
            issues.push(CompletionIssue::MissingTerminal);
        }
        if child_exit.code.is_none() {
            issues.push(CompletionIssue::MissingChildExit);
        }
        if let (Some(terminal), Some(actual)) = (&self.terminal, child_exit.code)
            && terminal.exit_code != actual
        {
            issues.push(CompletionIssue::ExitMismatch {
                reported: terminal.exit_code,
                actual,
            });
        }
        if let Some(interruption) = child_exit.interruption {
            issues.push(CompletionIssue::Interrupted(interruption));
        }
        let confirmed = issues.is_empty();
        let outcome = if confirmed {
            match self.terminal.as_ref().map(|terminal| terminal.result) {
                Some(RunResult::Succeeded) => CompletionOutcome::Succeeded,
                Some(RunResult::Failed) => CompletionOutcome::Failed,
                Some(RunResult::Cancelled) => CompletionOutcome::Cancelled,
                None => CompletionOutcome::Incomplete,
            }
        } else if child_exit.code == Some(130)
            || child_exit.interruption == Some(Interruption::CallerCancellation)
        {
            CompletionOutcome::Cancelled
        } else {
            CompletionOutcome::Incomplete
        };
        BuildCompletion {
            outcome,
            confirmed,
            run_id: self.run_id,
            accepted_events: self.sequence,
            terminal: self.terminal,
            child_exit,
            issues,
        }
    }

    fn check_error(&self) -> Result<(), StreamError> {
        match &self.error {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn poison<T>(&mut self, error: StreamError) -> Result<T, StreamError> {
        self.pending.clear();
        self.error = Some(error.clone());
        Err(error)
    }

    fn accept_decoded(
        &mut self,
        decoded: Result<Map<String, Value>, StreamError>,
    ) -> Result<BuildEvent, StreamError> {
        let result = decoded.and_then(|object| self.validate(object));
        match result {
            Ok(event) => {
                self.sequence = event.sequence;
                self.elapsed_ms = event.elapsed_ms;
                if self.run_id.is_none() {
                    self.run_id = Some(event.run_id.clone());
                }
                self.started |= event.kind == Some(KnownEventKind::RunStarted);
                self.saw_failure |= event.kind == Some(KnownEventKind::Error)
                    || (event.kind == Some(KnownEventKind::StageFinished)
                        && matches!(
                            event.data.get("result").and_then(Value::as_str),
                            Some("failed" | "blocked" | "cancelled")
                        ));
                if let Some(terminal) = &event.terminal {
                    self.terminal = Some(terminal.clone());
                }
                Ok(event)
            }
            Err(error) => self.poison(error),
        }
    }

    fn validate(&self, mut object: Map<String, Value>) -> Result<BuildEvent, StreamError> {
        let schema_version = envelope_uint(&mut object, "schema_version")?;
        if schema_version != 1 {
            return Err(StreamError::new(
                StreamErrorKind::UnsupportedSchema,
                format!("Unsupported build-event schema {schema_version}; expected version 1."),
            ));
        }
        let sequence = envelope_uint(&mut object, "sequence")?;
        let expected = self.sequence.checked_add(1).ok_or_else(|| {
            StreamError::new(
                StreamErrorKind::OutOfOrderSequence,
                "Sequence counter exhausted.",
            )
        })?;
        if sequence != expected {
            let kind = if sequence > expected {
                StreamErrorKind::MissingSequence
            } else if sequence == self.sequence && self.sequence != 0 {
                StreamErrorKind::DuplicateSequence
            } else {
                StreamErrorKind::OutOfOrderSequence
            };
            return Err(StreamError::new(
                kind,
                format!("Expected stdout event sequence {expected}, received {sequence}."),
            ));
        }
        let run_id = envelope_string(&mut object, "run_id", MAX_RUN_ID_BYTES)?;
        if self.run_id.as_ref().is_some_and(|id| *id != run_id) {
            return Err(StreamError::new(
                StreamErrorKind::WrongRun,
                "Stdout run_id changed; keep each child process on its own decoder.",
            ));
        }
        let elapsed_ms = envelope_uint(&mut object, "elapsed_ms")?;
        if elapsed_ms < self.elapsed_ms {
            return Err(StreamError::new(
                StreamErrorKind::ElapsedRegression,
                "Event elapsed_ms moved backwards within the same run.",
            ));
        }
        let event = envelope_string(&mut object, "event", MAX_EVENT_BYTES)?;
        let kind = KnownEventKind::from_name(&event);
        let data = match object.remove("data") {
            Some(Value::Object(data)) => data,
            _ => return Err(invalid_envelope("data must be a JSON object")),
        };
        if self.terminal.is_some() {
            let (kind, message) = if kind == Some(KnownEventKind::RunFinished) {
                (
                    StreamErrorKind::DuplicateTerminal,
                    "Duplicate run_finished record.",
                )
            } else {
                (
                    StreamErrorKind::EventAfterTerminal,
                    "Received an event after run_finished.",
                )
            };
            return Err(StreamError::new(kind, message));
        }
        if kind == Some(KnownEventKind::RunStarted) && self.started {
            return Err(StreamError::new(
                StreamErrorKind::DuplicateRunStarted,
                "Duplicate run_started record in one child process.",
            ));
        }
        let terminal = validate_data(kind, &data).map_err(|detail| {
            StreamError::new(
                StreamErrorKind::InvalidEventData,
                format!("Invalid {event} data at sequence {sequence}: {detail}."),
            )
        })?;
        if let Some(terminal) = &terminal {
            let expected = match terminal.exit_code {
                0 => RunResult::Succeeded,
                130 => RunResult::Cancelled,
                _ => RunResult::Failed,
            };
            if terminal.result != expected {
                return Err(StreamError::new(
                    StreamErrorKind::TerminalContradiction,
                    "run_finished result contradicts its exit_code (0 success, 130 cancellation).",
                ));
            }
            if terminal.result == RunResult::Succeeded && self.saw_failure {
                return Err(StreamError::new(
                    StreamErrorKind::TerminalContradiction,
                    "run_finished claims success after an error or failed/blocked/cancelled stage.",
                ));
            }
        }
        Ok(BuildEvent {
            schema_version,
            sequence,
            run_id,
            elapsed_ms,
            event,
            kind,
            data,
            extra: object,
            terminal,
        })
    }
}

fn decode_line(bytes: &[u8], limit: usize) -> Result<Map<String, Value>, StreamError> {
    if bytes.len() > limit {
        return Err(StreamError::new(
            StreamErrorKind::LineTooLong,
            format!("Stdout JSONL line exceeds the {limit}-byte wire limit."),
        ));
    }
    let line = std::str::from_utf8(bytes).map_err(|_| {
        StreamError::new(
            StreamErrorKind::InvalidUtf8,
            "Stdout JSONL line is not valid UTF-8.",
        )
    })?;
    let line = if let Some(line) = line.strip_suffix('\n') {
        line.strip_suffix('\r').unwrap_or(line)
    } else {
        line
    };
    if line.trim().is_empty() || line.contains(['\r', '\n']) {
        return Err(StreamError::new(
            StreamErrorKind::Framing,
            "Expected one nonempty JSONL stdout record; keep human stderr separate.",
        ));
    }
    let value: Value = serde_json::from_str(line).map_err(|error| {
        StreamError::new(
            StreamErrorKind::MalformedJson,
            format!("Malformed stdout JSONL (keep stderr separate): {error}"),
        )
    })?;
    match value {
        Value::Object(object) => Ok(object),
        _ => Err(invalid_envelope("record must be a JSON object")),
    }
}

fn invalid_envelope(detail: &str) -> StreamError {
    StreamError::new(
        StreamErrorKind::InvalidEnvelope,
        format!("Invalid envelope: {detail}."),
    )
}

fn envelope_uint(object: &mut Map<String, Value>, key: &str) -> Result<u64, StreamError> {
    object
        .remove(key)
        .and_then(|value| value.as_u64())
        .ok_or_else(|| invalid_envelope(&format!("{key} must be a nonnegative integer")))
}

fn envelope_string(
    object: &mut Map<String, Value>,
    key: &str,
    max_bytes: usize,
) -> Result<String, StreamError> {
    match object.remove(key) {
        Some(Value::String(value)) if !value.trim().is_empty() && value.len() <= max_bytes => {
            Ok(value)
        }
        _ => Err(invalid_envelope(&format!(
            "{key} must be a nonempty string of at most {max_bytes} bytes"
        ))),
    }
}

type DataResult<T> = Result<T, String>;

fn field<'a>(data: &'a Map<String, Value>, key: &str) -> DataResult<&'a Value> {
    data.get(key).ok_or_else(|| format!("missing {key}"))
}

fn text_field<'a>(data: &'a Map<String, Value>, key: &str, nonempty: bool) -> DataResult<&'a str> {
    let value = field(data, key)?
        .as_str()
        .ok_or_else(|| format!("{key} must be a string"))?;
    if nonempty && value.trim().is_empty() {
        return Err(format!("{key} must not be empty"));
    }
    Ok(value)
}

fn string_array(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|items| items.iter().all(Value::is_string))
}

fn nonnegative_number(value: &Value) -> bool {
    value
        .as_f64()
        .is_some_and(|number| number.is_finite() && number >= 0.0)
}

fn i32_number(value: &Value) -> bool {
    value
        .as_i64()
        .is_some_and(|number| i32::try_from(number).is_ok())
}

fn check(
    data: &Map<String, Value>,
    key: &str,
    predicate: impl FnOnce(&Value) -> bool,
    expected: &str,
) -> DataResult<()> {
    if !predicate(field(data, key)?) {
        return Err(format!("{key} must be {expected}"));
    }
    Ok(())
}

fn optional(
    data: &Map<String, Value>,
    key: &str,
    predicate: impl FnOnce(&Value) -> bool,
    expected: &str,
) -> DataResult<()> {
    if let Some(value) = data.get(key)
        && !predicate(value)
    {
        return Err(format!("{key} must be {expected} when present"));
    }
    Ok(())
}

fn enum_field<'a>(
    data: &'a Map<String, Value>,
    key: &str,
    choices: &[&str],
) -> DataResult<&'a str> {
    let value = text_field(data, key, true)?;
    if !choices.contains(&value) {
        return Err(format!("{key} must be one of {}", choices.join(", ")));
    }
    Ok(value)
}

fn validate_data(
    kind: Option<KnownEventKind>,
    data: &Map<String, Value>,
) -> DataResult<Option<TerminalInfo>> {
    let Some(kind) = kind else {
        return Ok(None);
    };
    match kind {
        KnownEventKind::RunStarted => {
            text_field(data, "operation", true)?;
            for key in ["root", "config"] {
                optional(data, key, Value::is_string, "a string")?;
            }
        }
        KnownEventKind::StageDeclared => {
            text_field(data, "name", true)?;
            text_field(data, "owner", false)?;
            for key in ["command", "dependencies"] {
                check(data, key, string_array, "an array of strings")?;
            }
            for key in ["default", "gpu", "verify"] {
                check(data, key, Value::is_boolean, "a boolean")?;
            }
            optional(data, "deterministic", Value::is_boolean, "a boolean")?;
            optional(
                data,
                "mem_gb",
                nonnegative_number,
                "a finite nonnegative number",
            )?;
            optional(data, "locks", string_array, "an array of strings")?;
        }
        KnownEventKind::Status => {
            for key in ["config", "root", "log_dir"] {
                text_field(data, key, false)?;
            }
            check(data, "stages", Value::is_array, "an array")?;
            for stage in data["stages"].as_array().into_iter().flatten() {
                let stage = stage
                    .as_object()
                    .ok_or_else(|| "stages entries must be objects".to_owned())?;
                text_field(stage, "name", true)?;
                for key in ["inputs", "outputs"] {
                    optional(
                        stage,
                        key,
                        |value| value.as_u64().is_some(),
                        "a nonnegative integer",
                    )?;
                }
                optional(
                    stage,
                    "last_ok",
                    |value| value.is_null() || value.is_boolean(),
                    "a boolean or null",
                )?;
            }
            check(data, "pinned", Value::is_array, "an array")?;
            for key in ["notes", "games"] {
                optional(data, key, string_array, "an array of strings")?;
            }
            optional(
                data,
                "running_pid",
                |value| value.is_null() || value.as_u64().is_some_and(|pid| pid <= u32::MAX.into()),
                "a u32 or null",
            )?;
        }
        KnownEventKind::StagePlanned => {
            text_field(data, "name", true)?;
            enum_field(data, "result", &["run", "maybe", "skip"])?;
            text_field(data, "reason", false)?;
            optional(
                data,
                "estimate_mb",
                nonnegative_number,
                "a finite nonnegative number",
            )?;
        }
        KnownEventKind::StageStarted => {
            text_field(data, "name", true)?;
            check(
                data,
                "pid",
                |value| {
                    value
                        .as_u64()
                        .is_some_and(|pid| pid > 0 && pid <= u32::MAX.into())
                },
                "a positive u32",
            )?;
            text_field(data, "reason", false)?;
            text_field(data, "log_path", false)?;
        }
        KnownEventKind::StageFinished => {
            text_field(data, "name", true)?;
            enum_field(
                data,
                "result",
                &["succeeded", "failed", "skipped", "cancelled", "blocked"],
            )?;
            for key in ["seconds", "peak_mb"] {
                check(data, key, nonnegative_number, "a finite nonnegative number")?;
            }
            optional(
                data,
                "exit_code",
                |value| value.is_null() || i32_number(value),
                "an i32 or null",
            )?;
            for key in ["error", "reason"] {
                optional(data, key, Value::is_string, "a string")?;
            }
            for key in ["inputs", "outputs", "output_count"] {
                optional(
                    data,
                    key,
                    |value| value.as_u64().is_some(),
                    "a nonnegative integer",
                )?;
            }
            optional(data, "trace_complete", Value::is_boolean, "a boolean")?;
            for key in ["changed_during_run", "fallbacks"] {
                optional(data, key, string_array, "an array of strings")?;
            }
            optional(
                data,
                "rust_stamp",
                |value| value.is_null() || value.is_string(),
                "a string or null",
            )?;
        }
        KnownEventKind::Progress => {
            for key in ["selected", "completed", "running"] {
                check(
                    data,
                    key,
                    |value| value.as_u64().is_some(),
                    "a nonnegative integer",
                )?;
            }
        }
        KnownEventKind::Warning | KnownEventKind::Error => {
            text_field(data, "message", true)?;
        }
        KnownEventKind::RunFinished => {
            let result = match enum_field(data, "result", &["succeeded", "failed", "cancelled"])? {
                "succeeded" => RunResult::Succeeded,
                "cancelled" => RunResult::Cancelled,
                _ => RunResult::Failed,
            };
            check(data, "exit_code", i32_number, "an i32")?;
            check(
                data,
                "elapsed_seconds",
                nonnegative_number,
                "a finite nonnegative number",
            )?;
            // The typed getters are justified by the checks above, without panics.
            let exit_code = field(data, "exit_code")?
                .as_i64()
                .and_then(|code| i32::try_from(code).ok())
                .ok_or_else(|| "exit_code must be an i32".to_owned())?;
            let elapsed_seconds = field(data, "elapsed_seconds")?
                .as_f64()
                .ok_or_else(|| "elapsed_seconds must be a number".to_owned())?;
            return Ok(Some(TerminalInfo {
                result,
                exit_code,
                elapsed_seconds,
            }));
        }
    }
    Ok(None)
}
