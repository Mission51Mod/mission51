//! Synthetic wire-contract tests against the actual N decoder, without a GUI.
#[path = "../src/build_events.rs"]
mod build_events;

use build_events::*;
use serde_json::{Value, json};

fn record(sequence: u64, event: &str, data: Value) -> Value {
    json!({
        "schema_version": 1, "sequence": sequence, "run_id": "fixture-run",
        "elapsed_ms": sequence * 10, "event": event, "data": data
    })
}

fn started(sequence: u64) -> Value {
    record(
        sequence,
        "run_started",
        json!({"operation": "build", "root": "H:/fixture", "config": "stages.toml"}),
    )
}

fn terminal(sequence: u64, result: &str, code: i32) -> Value {
    record(
        sequence,
        "run_finished",
        json!({"result": result, "exit_code": code, "elapsed_seconds": 0.5, "stages": {"build": "succeeded"}}),
    )
}

fn push(stream: &mut BuildEventStream, value: Value) -> BuildEvent {
    stream.push_line(&value.to_string()).expect("valid fixture")
}

fn start_stream() -> BuildEventStream {
    let mut stream = BuildEventStream::new();
    push(&mut stream, started(1));
    stream
}

fn completed(result: &str, reported: i32, actual: ChildExit) -> BuildCompletion {
    let mut stream = start_stream();
    push(&mut stream, terminal(2, result, reported));
    stream.finish(actual)
}

fn assert_poisoned(value: Value, expected: StreamErrorKind) {
    let mut stream = BuildEventStream::new();
    let error = stream.push_line(&value.to_string()).unwrap_err();
    assert_eq!(error.kind, expected, "{error}");
    assert_eq!(
        stream.push_line(&started(1).to_string()).unwrap_err(),
        error
    );
    assert_eq!(stream.accepted_events(), 0);
    let report = stream.finish(ChildExit::exited(0));
    assert_eq!(report.outcome, CompletionOutcome::Incomplete);
    assert!(!report.confirmed);
    assert!(report.issues.contains(&CompletionIssue::Protocol(error)));
}

fn known_fixture(event: &str) -> Value {
    match event {
        "run_started" => {
            json!({"operation": "build", "root": "H:/fixture", "config": "stages.toml"})
        }
        "stage_declared" => {
            json!({"name": "mesh", "owner": "C", "command": ["python", "mesh.py"], "dependencies": ["input"], "default": true, "gpu": false, "verify": true, "deterministic": true, "mem_gb": 0.5, "locks": ["mesh"]})
        }
        "status" => {
            json!({"root": "H:/fixture", "config": "stages.toml", "log_dir": "logs", "stages": [{"name": "mesh", "inputs": 4, "outputs": 3, "last_ok": null}], "pinned": [], "running_pid": null, "notes": [], "games": [], "rust_tools": {"Ok": false}})
        }
        "stage_planned" => {
            json!({"name": "mesh", "result": "run", "reason": "input changed", "estimate_mb": 512})
        }
        "stage_started" => {
            json!({"name": "mesh", "pid": 123, "reason": "input changed", "log_path": "logs/mesh.log"})
        }
        "stage_finished" => {
            json!({"name": "mesh", "result": "succeeded", "seconds": 0.3, "peak_mb": 50.0, "exit_code": 0, "inputs": 4, "outputs": 3, "trace_complete": true, "changed_during_run": [], "fallbacks": [], "rust_stamp": null})
        }
        "progress" => json!({"selected": 2, "completed": 1, "running": 1}),
        "warning" => json!({"message": "pin unavailable", "context": {"stage": "mesh"}}),
        "error" => json!({"message": "stage failed", "context": {"exit_code": 2}}),
        "run_finished" => json!({"result": "succeeded", "exit_code": 0, "elapsed_seconds": 0.5}),
        _ => panic!("unexpected fixture"),
    }
}

#[test]
fn real_producer_shapes_form_a_confirmed_success() {
    let mut stream = BuildEventStream::new();
    let events = [
        "run_started",
        "stage_declared",
        "status",
        "stage_planned",
        "stage_started",
        "progress",
        "stage_finished",
        "warning",
        "run_finished",
    ];
    for (index, name) in events.iter().enumerate() {
        let event = push(
            &mut stream,
            record(index as u64 + 1, name, known_fixture(name)),
        );
        assert!(event.kind.is_some());
        assert_eq!(event.terminal.is_some(), *name == "run_finished");
    }
    assert_eq!(stream.buffered_bytes(), 0);
    let report = stream.finish(ChildExit::exited(0));
    assert_eq!(report.outcome, CompletionOutcome::Succeeded);
    assert!(report.confirmed);
    assert_eq!(report.accepted_events, 9);
    assert_eq!(report.run_id.as_deref(), Some("fixture-run"));
    assert!(report.issues.is_empty());
}

#[test]
fn producer_optional_fields_and_null_exit_codes_are_accepted() {
    for data in [
        json!({"name": "mesh", "result": "failed", "seconds": 0, "peak_mb": 0, "error": "spawn failed"}),
        json!({"name": "mesh", "result": "failed", "seconds": 1.1, "peak_mb": 50, "exit_code": null, "error": "killed"}),
        json!({"name": "mesh", "result": "blocked", "seconds": 0, "peak_mb": 0, "reason": "dependency"}),
        json!({"name": "mesh", "result": "cancelled", "seconds": 0, "peak_mb": 0, "reason": "cancel file"}),
        json!({"name": "mesh", "result": "skipped", "seconds": 0, "peak_mb": 0, "reason": "clean"}),
    ] {
        let mut stream = start_stream();
        push(&mut stream, record(2, "stage_finished", data));
    }
    let mut stream = start_stream();
    push(
        &mut stream,
        record(
            2,
            "stage_planned",
            json!({"name": "mesh", "result": "run", "reason": "dirty"}),
        ),
    );
    push(
        &mut stream,
        record(3, "warning", json!({"message": "warning without context"})),
    );
    push(&mut stream, terminal(4, "succeeded", 0));
    assert!(stream.finish(ChildExit::exited(0)).confirmed);
}

#[test]
fn future_fields_and_kinds_survive_without_breaking_sequence() {
    let mut stream = BuildEventStream::new();
    let mut future = record(1, "future_bootstrap", json!({"nested": [1, {"new": true}]}));
    future["future_envelope"] = json!({"version": 9});
    let event = push(&mut stream, future);
    assert_eq!(event.kind, None);
    assert_eq!(event.data["nested"][1]["new"], true);
    assert_eq!(event.extra["future_envelope"]["version"], 9);
    push(&mut stream, started(2));
    let mut finished = terminal(3, "succeeded", 0);
    finished["data"]["new_summary"] = json!([1, 2, 3]);
    let event = push(&mut stream, finished);
    assert_eq!(event.data["new_summary"], json!([1, 2, 3]));
    assert_eq!(event.terminal.unwrap().result, RunResult::Succeeded);
    assert_eq!(
        stream.finish(ChildExit::exited(0)).outcome,
        CompletionOutcome::Succeeded
    );
}

#[test]
fn clean_failed_and_cooperative_cancelled_runs_are_distinct() {
    for (result, code, outcome) in [
        ("failed", 2, CompletionOutcome::Failed),
        ("cancelled", 130, CompletionOutcome::Cancelled),
    ] {
        let mut stream = start_stream();
        push(
            &mut stream,
            record(2, "error", json!({"message": "operation stopped"})),
        );
        push(&mut stream, terminal(3, result, code));
        let report = stream.finish(ChildExit::exited(code));
        assert_eq!(report.outcome, outcome);
        assert!(report.confirmed);
    }
}

#[test]
fn a_cancel_request_that_loses_the_completion_race_is_still_success() {
    // Merely creating a cancel marker is not an observed interruption.
    let report = completed("succeeded", 0, ChildExit::exited(0));
    assert_eq!(report.outcome, CompletionOutcome::Succeeded);
    assert!(report.confirmed);
}

#[test]
fn actual_child_exit_is_required_even_with_a_valid_terminal() {
    for (result, reported, actual) in [("succeeded", 0, 1), ("failed", 1, 0), ("cancelled", 130, 0)]
    {
        let report = completed(result, reported, ChildExit::exited(actual));
        assert_eq!(report.outcome, CompletionOutcome::Incomplete);
        assert!(!report.confirmed);
        assert!(
            report
                .issues
                .contains(&CompletionIssue::ExitMismatch { reported, actual })
        );
    }
    let report = completed(
        "succeeded",
        0,
        ChildExit {
            code: None,
            interruption: None,
        },
    );
    assert_eq!(report.outcome, CompletionOutcome::Incomplete);
    assert!(report.issues.contains(&CompletionIssue::MissingChildExit));
}

#[test]
fn actual_exit_130_cannot_be_mistaken_for_success() {
    let report = completed("succeeded", 0, ChildExit::exited(130));
    assert_eq!(report.outcome, CompletionOutcome::Cancelled);
    assert!(!report.confirmed);
    assert!(report.issues.contains(&CompletionIssue::ExitMismatch {
        reported: 0,
        actual: 130
    }));
}

#[test]
fn missing_terminal_never_means_success_or_a_confirmed_failure() {
    for code in [0, 1, 130] {
        let report = start_stream().finish(ChildExit::exited(code));
        let expected = if code == 130 {
            CompletionOutcome::Cancelled
        } else {
            CompletionOutcome::Incomplete
        };
        assert_eq!(report.outcome, expected);
        assert!(!report.confirmed);
        assert!(report.issues.contains(&CompletionIssue::MissingTerminal));
    }
}

#[test]
fn forced_termination_broken_pipe_and_observed_cancellation_need_caller_context() {
    for interruption in [
        Interruption::ForcedTermination,
        Interruption::BrokenOutputTransport,
        Interruption::CallerCancellation,
    ] {
        let report = start_stream().finish(ChildExit {
            code: None,
            interruption: Some(interruption),
        });
        let expected = if interruption == Interruption::CallerCancellation {
            CompletionOutcome::Cancelled
        } else {
            CompletionOutcome::Incomplete
        };
        assert_eq!(report.outcome, expected);
        assert!(!report.confirmed);
        assert!(
            report
                .issues
                .contains(&CompletionIssue::Interrupted(interruption))
        );
        assert!(report.issues.contains(&CompletionIssue::MissingTerminal));
    }
}

#[test]
fn observed_interruption_blocks_even_an_otherwise_matching_success() {
    for interruption in [
        Interruption::ForcedTermination,
        Interruption::BrokenOutputTransport,
        Interruption::CallerCancellation,
    ] {
        let report = completed(
            "succeeded",
            0,
            ChildExit {
                code: Some(0),
                interruption: Some(interruption),
            },
        );
        assert!(!report.confirmed);
        assert_ne!(report.outcome, CompletionOutcome::Succeeded);
    }
}

#[test]
fn terminal_without_run_started_cannot_confirm_success() {
    let mut stream = BuildEventStream::new();
    push(&mut stream, terminal(1, "succeeded", 0));
    let report = stream.finish(ChildExit::exited(0));
    assert_eq!(report.outcome, CompletionOutcome::Incomplete);
    assert!(report.issues.contains(&CompletionIssue::MissingRunStarted));
}

#[test]
fn terminal_result_and_reported_exit_must_agree() {
    for (result, code) in [
        ("succeeded", 2),
        ("succeeded", 130),
        ("failed", 0),
        ("failed", 130),
        ("cancelled", 0),
        ("cancelled", 1),
    ] {
        let mut stream = start_stream();
        let error = stream
            .push_line(&terminal(2, result, code).to_string())
            .unwrap_err();
        assert_eq!(error.kind, StreamErrorKind::TerminalContradiction);
        assert_ne!(
            stream.finish(ChildExit::exited(0)).outcome,
            CompletionOutcome::Succeeded
        );
    }
}

#[test]
fn prior_error_or_failed_stage_contradicts_success() {
    let cases = [
        ("error", json!({"message": "failed"})),
        (
            "stage_finished",
            json!({"name": "mesh", "result": "failed", "seconds": 0, "peak_mb": 0}),
        ),
        (
            "stage_finished",
            json!({"name": "mesh", "result": "blocked", "seconds": 0, "peak_mb": 0}),
        ),
        (
            "stage_finished",
            json!({"name": "mesh", "result": "cancelled", "seconds": 0, "peak_mb": 0}),
        ),
    ];
    for (name, data) in cases {
        let mut stream = start_stream();
        push(&mut stream, record(2, name, data));
        assert_eq!(
            stream
                .push_line(&terminal(3, "succeeded", 0).to_string())
                .unwrap_err()
                .kind,
            StreamErrorKind::TerminalContradiction
        );
        assert_eq!(
            stream.finish(ChildExit::exited(0)).outcome,
            CompletionOutcome::Incomplete
        );
    }
}

#[test]
fn duplicate_terminal_and_events_after_terminal_poison_a_previous_success() {
    for (value, kind) in [
        (
            terminal(3, "succeeded", 0),
            StreamErrorKind::DuplicateTerminal,
        ),
        (
            record(3, "future_kind", json!({})),
            StreamErrorKind::EventAfterTerminal,
        ),
        (
            record(
                3,
                "progress",
                json!({"selected": 1, "completed": 1, "running": 0}),
            ),
            StreamErrorKind::EventAfterTerminal,
        ),
    ] {
        let mut stream = start_stream();
        push(&mut stream, terminal(2, "succeeded", 0));
        assert_eq!(stream.push_line(&value.to_string()).unwrap_err().kind, kind);
        let report = stream.finish(ChildExit::exited(0));
        assert_eq!(report.outcome, CompletionOutcome::Incomplete);
        assert!(!report.confirmed);
        assert_eq!(report.accepted_events, 2);
    }
}

#[test]
fn duplicate_run_started_is_an_error() {
    let mut stream = start_stream();
    assert_eq!(
        stream.push_line(&started(2).to_string()).unwrap_err().kind,
        StreamErrorKind::DuplicateRunStarted
    );
}

#[test]
fn missing_duplicate_and_out_of_order_sequences_are_distinct() {
    assert_poisoned(started(2), StreamErrorKind::MissingSequence);
    assert_poisoned(started(0), StreamErrorKind::OutOfOrderSequence);
    for (sequence, kind) in [
        (1, StreamErrorKind::DuplicateSequence),
        (0, StreamErrorKind::OutOfOrderSequence),
        (3, StreamErrorKind::MissingSequence),
    ] {
        let mut stream = start_stream();
        let value = record(sequence, "future_kind", json!({}));
        assert_eq!(stream.push_line(&value.to_string()).unwrap_err().kind, kind);
        assert_eq!(stream.accepted_events(), 1);
        assert_eq!(
            stream.finish(ChildExit::exited(0)).outcome,
            CompletionOutcome::Incomplete
        );
    }
}

#[test]
fn a_future_kind_still_advances_sequence_integrity() {
    let mut stream = start_stream();
    push(&mut stream, record(2, "future_kind", json!({})));
    let error = stream
        .push_line(&terminal(4, "succeeded", 0).to_string())
        .unwrap_err();
    assert_eq!(error.kind, StreamErrorKind::MissingSequence);
}

#[test]
fn run_ids_are_nonempty_bounded_and_cannot_change() {
    for id in ["".to_owned(), "  \t".to_owned(), "x".repeat(257)] {
        let mut value = started(1);
        value["run_id"] = json!(id);
        assert_poisoned(value, StreamErrorKind::InvalidEnvelope);
    }
    let mut stream = start_stream();
    let mut value = terminal(2, "succeeded", 0);
    value["run_id"] = json!("other-child");
    assert_eq!(
        stream.push_line(&value.to_string()).unwrap_err().kind,
        StreamErrorKind::WrongRun
    );
    assert_eq!(
        stream.finish(ChildExit::exited(0)).outcome,
        CompletionOutcome::Incomplete
    );
}

#[test]
fn schema_version_must_be_integer_one_even_for_future_kinds() {
    for schema in [json!(0), json!(2)] {
        let mut value = record(1, "future_kind", json!({}));
        value["schema_version"] = schema;
        assert_poisoned(value, StreamErrorKind::UnsupportedSchema);
    }
    for schema in [Value::Null, json!("1"), json!(1.0), json!(-1)] {
        let mut value = started(1);
        value["schema_version"] = schema;
        assert_poisoned(value, StreamErrorKind::InvalidEnvelope);
    }
}

#[test]
fn required_envelope_fields_reject_missing_and_wrong_types() {
    for key in [
        "schema_version",
        "sequence",
        "run_id",
        "elapsed_ms",
        "event",
        "data",
    ] {
        let mut value = started(1);
        value.as_object_mut().unwrap().remove(key);
        assert_poisoned(value, StreamErrorKind::InvalidEnvelope);
    }
    for (key, bad) in [
        ("sequence", json!(-1)),
        ("sequence", json!(1.5)),
        ("elapsed_ms", json!(-1)),
        ("elapsed_ms", json!(0.5)),
        ("run_id", json!(1)),
        ("event", json!("")),
        ("event", json!("x".repeat(129))),
        ("data", json!([])),
        ("data", Value::Null),
    ] {
        let mut value = started(1);
        value[key] = bad;
        assert_poisoned(value, StreamErrorKind::InvalidEnvelope);
    }
    assert_poisoned(json!([]), StreamErrorKind::InvalidEnvelope);
}

#[test]
fn elapsed_time_may_repeat_but_cannot_regress() {
    let mut stream = start_stream();
    let mut value = record(2, "future_kind", json!({}));
    value["elapsed_ms"] = json!(10);
    push(&mut stream, value);
    let mut value = terminal(3, "succeeded", 0);
    value["elapsed_ms"] = json!(9);
    assert_eq!(
        stream.push_line(&value.to_string()).unwrap_err().kind,
        StreamErrorKind::ElapsedRegression
    );
}

#[test]
fn every_known_event_validates_its_required_fields() {
    for (name, keys) in [
        ("run_started", vec!["operation"]),
        (
            "stage_declared",
            vec![
                "name",
                "owner",
                "command",
                "dependencies",
                "default",
                "gpu",
                "verify",
            ],
        ),
        (
            "status",
            vec!["root", "config", "log_dir", "stages", "pinned"],
        ),
        ("stage_planned", vec!["name", "result", "reason"]),
        ("stage_started", vec!["name", "pid", "reason", "log_path"]),
        (
            "stage_finished",
            vec!["name", "result", "seconds", "peak_mb"],
        ),
        ("progress", vec!["selected", "completed", "running"]),
        ("warning", vec!["message"]),
        ("error", vec!["message"]),
        (
            "run_finished",
            vec!["result", "exit_code", "elapsed_seconds"],
        ),
    ] {
        for key in keys {
            for remove in [false, true] {
                let mut data = known_fixture(name);
                if remove {
                    data.as_object_mut().unwrap().remove(key);
                } else {
                    data[key] = Value::Null;
                }
                let mut stream = BuildEventStream::new();
                let error = stream
                    .push_line(&record(1, name, data).to_string())
                    .unwrap_err();
                assert_eq!(
                    error.kind,
                    StreamErrorKind::InvalidEventData,
                    "{name}.{key}: {error}"
                );
                assert!(error.message.contains(key));
            }
        }
    }
}

#[test]
fn known_optional_fields_reject_wrong_types_and_ranges() {
    let cases = [
        ("run_started", "root", json!(5)),
        ("stage_declared", "command", json!("python mesh.py")),
        ("stage_declared", "dependencies", json!([1])),
        ("stage_declared", "mem_gb", json!(-1)),
        ("stage_declared", "deterministic", json!("yes")),
        ("stage_declared", "locks", json!([null])),
        ("stage_planned", "result", json!("succeeded")),
        ("stage_planned", "estimate_mb", json!(-1)),
        ("stage_started", "pid", json!(0)),
        ("stage_started", "pid", json!(4294967296_u64)),
        ("stage_finished", "result", json!("running")),
        ("stage_finished", "seconds", json!(-0.1)),
        ("stage_finished", "peak_mb", json!(-1)),
        ("stage_finished", "exit_code", json!(2147483648_u64)),
        ("stage_finished", "exit_code", json!(1.5)),
        ("stage_finished", "error", json!([])),
        ("stage_finished", "inputs", json!(-1)),
        ("stage_finished", "outputs", json!(1.5)),
        ("stage_finished", "trace_complete", json!("true")),
        ("stage_finished", "fallbacks", json!([false])),
        ("stage_finished", "changed_during_run", json!("path")),
        ("stage_finished", "rust_stamp", json!(7)),
        ("progress", "selected", json!(-1)),
        ("progress", "running", json!(0.5)),
        ("warning", "message", json!(" ")),
        ("run_finished", "exit_code", json!(2147483648_u64)),
        ("run_finished", "exit_code", json!(0.0)),
        ("run_finished", "elapsed_seconds", json!(-1)),
        ("status", "running_pid", json!(-1)),
        ("status", "notes", json!([1])),
        ("status", "stages", json!(["mesh"])),
        ("status", "stages", json!([{"name": "mesh", "inputs": -1}])),
        (
            "status",
            "stages",
            json!([{"name": "mesh", "last_ok": "yes"}]),
        ),
    ];
    for (name, key, bad) in cases {
        let mut data = known_fixture(name);
        data[key] = bad;
        let mut stream = BuildEventStream::new();
        assert_eq!(
            stream
                .push_line(&record(1, name, data).to_string())
                .unwrap_err()
                .kind,
            StreamErrorKind::InvalidEventData,
            "{name}.{key}"
        );
    }
}

#[test]
fn unrelated_progress_counts_are_not_an_invented_protocol_constraint() {
    let mut stream = start_stream();
    push(
        &mut stream,
        record(
            2,
            "progress",
            json!({"selected": 0, "completed": 100, "running": 3}),
        ),
    );
}

#[test]
fn framed_lines_accept_lf_crlf_and_caller_proven_no_delimiter() {
    for suffix in ["", "\n", "\r\n"] {
        let mut stream = BuildEventStream::new();
        stream
            .push_line(&format!("{}{suffix}", started(1)))
            .unwrap();
        stream
            .push_line(&format!("{}{suffix}", terminal(2, "succeeded", 0)))
            .unwrap();
        assert!(stream.finish(ChildExit::exited(0)).confirmed);
    }
}

#[test]
fn blank_multiline_and_lone_cr_are_framing_errors() {
    for line in [
        "".to_owned(),
        " \t\n".to_owned(),
        format!("{}\r", started(1)),
        format!("{}\n{}", started(1), terminal(2, "succeeded", 0)),
    ] {
        let mut stream = BuildEventStream::new();
        assert_eq!(
            stream.push_line(&line).unwrap_err().kind,
            StreamErrorKind::Framing
        );
    }
}

#[test]
fn malformed_truncated_json_and_human_stderr_are_actionable_errors() {
    for line in [
        "{",
        "{\"schema_version\":1",
        "building stage mesh...",
        "{} trailing text",
        "{}{}",
    ] {
        let mut stream = BuildEventStream::new();
        let error = stream.push_line(line).unwrap_err();
        assert_eq!(error.kind, StreamErrorKind::MalformedJson);
        assert!(error.message.contains("stdout") && error.message.contains("stderr"));
        assert_eq!(
            stream.finish(ChildExit::exited(0)).outcome,
            CompletionOutcome::Incomplete
        );
    }
}

#[test]
fn utf8_crlf_and_multiple_lines_work_at_every_possible_chunk_split() {
    let wire = format!(
        "{}\r\n{}\n{}\r\n",
        started(1),
        record(2, "warning", json!({"message": "île 森 🌳"})),
        terminal(3, "succeeded", 0)
    );
    for split in 0..=wire.len() {
        let mut stream = BuildEventStream::new();
        let mut events = Vec::new();
        stream
            .push_bytes(&wire.as_bytes()[..split], |event| events.push(event))
            .unwrap();
        stream
            .push_bytes(&wire.as_bytes()[split..], |event| events.push(event))
            .unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[1].data["message"], "île 森 🌳");
        assert_eq!(stream.buffered_bytes(), 0);
        assert!(
            stream.finish(ChildExit::exited(0)).confirmed,
            "split={split}"
        );
    }
}

#[test]
fn byte_at_a_time_framing_preserves_escaped_newlines() {
    let wire = format!(
        "{}\n{}\n{}\n",
        started(1),
        record(
            2,
            "warning",
            json!({"message": "first\nsecond\r\nthird 🌳"})
        ),
        terminal(3, "succeeded", 0)
    );
    let mut stream = BuildEventStream::new();
    let mut count = 0;
    for byte in wire.as_bytes() {
        stream.push_bytes(&[*byte], |_| count += 1).unwrap();
    }
    assert_eq!(count, 3);
    assert!(stream.finish(ChildExit::exited(0)).confirmed);
}

#[test]
fn invalid_utf8_is_rejected_only_when_the_line_is_complete() {
    let mut stream = BuildEventStream::new();
    stream
        .push_bytes(&[0xff], |_| panic!("partial line emitted"))
        .unwrap();
    assert_eq!(stream.buffered_bytes(), 1);
    assert_eq!(
        stream
            .push_bytes(b"\n", |_| panic!("invalid event"))
            .unwrap_err()
            .kind,
        StreamErrorKind::InvalidUtf8
    );
    assert_eq!(stream.buffered_bytes(), 0);
    assert_eq!(
        stream.finish(ChildExit::exited(0)).outcome,
        CompletionOutcome::Incomplete
    );
}

#[test]
fn eof_with_a_partial_or_undelimited_record_is_truncated() {
    for tail in ["{".to_owned(), terminal(2, "succeeded", 0).to_string()] {
        let mut stream = BuildEventStream::new();
        stream
            .push_bytes(format!("{}\n", started(1)).as_bytes(), |_| {})
            .unwrap();
        stream
            .push_bytes(tail.as_bytes(), |_| panic!("undelimited event emitted"))
            .unwrap();
        let report = stream.finish(ChildExit::exited(0));
        assert_eq!(report.outcome, CompletionOutcome::Incomplete);
        assert!(
            report
                .issues
                .contains(&CompletionIssue::TruncatedLine { bytes: tail.len() })
        );
        assert!(report.issues.contains(&CompletionIssue::MissingTerminal));
    }
}

#[test]
fn bytes_after_a_valid_terminal_still_prevent_success_at_eof() {
    let wire = format!("{}\n{}\n{{", started(1), terminal(2, "succeeded", 0));
    let mut stream = BuildEventStream::new();
    stream.push_bytes(wire.as_bytes(), |_| {}).unwrap();
    let report = stream.finish(ChildExit::exited(0));
    assert_eq!(report.outcome, CompletionOutcome::Incomplete);
    assert!(
        report
            .issues
            .contains(&CompletionIssue::TruncatedLine { bytes: 1 })
    );
}

#[test]
fn mixing_partial_raw_and_framed_input_is_rejected() {
    let mut stream = BuildEventStream::new();
    stream.push_bytes(b"{", |_| {}).unwrap();
    assert_eq!(
        stream.push_line(&started(1).to_string()).unwrap_err().kind,
        StreamErrorKind::Framing
    );
    assert_eq!(
        stream.finish(ChildExit::exited(0)).outcome,
        CompletionOutcome::Incomplete
    );
}

#[test]
fn oversize_lines_are_bounded_for_direct_and_chunked_input() {
    assert!(BuildEventStream::with_max_line_bytes(0).is_err());
    assert!(BuildEventStream::with_max_line_bytes(MAX_LINE_BYTES + 1).is_err());
    let mut stream = BuildEventStream::with_max_line_bytes(64).unwrap();
    stream.push_bytes(&[b'x'; 32], |_| {}).unwrap();
    stream.push_bytes(&[b'x'; 32], |_| {}).unwrap();
    assert_eq!(stream.buffered_bytes(), 64);
    assert_eq!(
        stream.push_bytes(b"\n", |_| {}).unwrap_err().kind,
        StreamErrorKind::LineTooLong
    );
    assert_eq!(stream.buffered_bytes(), 0);
    let mut stream = BuildEventStream::with_max_line_bytes(64).unwrap();
    assert_eq!(
        stream.push_line(&"x".repeat(65)).unwrap_err().kind,
        StreamErrorKind::LineTooLong
    );
    let mut stream = BuildEventStream::with_max_line_bytes(64).unwrap();
    assert_eq!(
        stream.push_bytes(&[b'x'; 4096], |_| {}).unwrap_err().kind,
        StreamErrorKind::LineTooLong
    );
    assert_eq!(stream.buffered_bytes(), 0);
}

#[test]
fn line_limit_counts_delimiters() {
    let line = started(1).to_string();
    let mut stream = BuildEventStream::with_max_line_bytes(line.len() + 1).unwrap();
    stream
        .push_bytes(format!("{line}\n").as_bytes(), |_| {})
        .unwrap();
    let mut stream = BuildEventStream::with_max_line_bytes(line.len()).unwrap();
    assert_eq!(
        stream
            .push_bytes(format!("{line}\n").as_bytes(), |_| {})
            .unwrap_err()
            .kind,
        StreamErrorKind::LineTooLong
    );
}

#[test]
fn first_bad_record_stops_the_chunk_and_poison_is_sticky() {
    let wire = format!(
        "{}\nnot json\n{}\n",
        started(1),
        terminal(2, "succeeded", 0)
    );
    let mut stream = BuildEventStream::new();
    let mut count = 0;
    let first = stream
        .push_bytes(wire.as_bytes(), |_| count += 1)
        .unwrap_err();
    assert_eq!(count, 1);
    assert_eq!(first.kind, StreamErrorKind::MalformedJson);
    assert_eq!(
        stream
            .push_bytes(
                format!("{}\n", terminal(2, "succeeded", 0)).as_bytes(),
                |_| {}
            )
            .unwrap_err(),
        first
    );
    assert_eq!(
        stream.finish(ChildExit::exited(0)).outcome,
        CompletionOutcome::Incomplete
    );
}

#[test]
fn long_stream_delivers_immediately_without_retaining_event_history() {
    let mut stream = BuildEventStream::new();
    let mut count = 0;
    stream
        .push_bytes(format!("{}\n", started(1)).as_bytes(), |_| count += 1)
        .unwrap();
    for sequence in 2..=10_001 {
        let line = format!(
            "{}\n",
            record(
                sequence,
                "progress",
                json!({"selected": 10_000, "completed": sequence - 2, "running": 1})
            )
        );
        stream.push_bytes(line.as_bytes(), |_| count += 1).unwrap();
        assert_eq!(stream.buffered_bytes(), 0);
        assert_eq!(stream.accepted_events(), sequence);
    }
    stream
        .push_bytes(
            format!("{}\n", terminal(10_002, "succeeded", 0)).as_bytes(),
            |_| count += 1,
        )
        .unwrap();
    assert_eq!(count, 10_002);
    let report = stream.finish(ChildExit::exited(0));
    assert!(report.confirmed);
    assert_eq!(report.accepted_events, count);
    assert!(report.issues.is_empty());
}

#[test]
fn retained_error_is_bounded_even_with_large_unsupported_event_names() {
    let mut stream = BuildEventStream::new();
    let mut value = record(1, &"é".repeat(64), json!({}));
    value["data"] = Value::Null;
    let error = stream.push_line(&value.to_string()).unwrap_err();
    assert!(error.message.len() <= 1024);
    let report = stream.finish(ChildExit {
        code: None,
        interruption: Some(Interruption::BrokenOutputTransport),
    });
    assert!(report.issues.len() <= 7);
    assert_eq!(report.outcome, CompletionOutcome::Incomplete);
}
