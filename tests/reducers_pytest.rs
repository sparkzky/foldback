/// Pytest reducer tests — strictly TDD: RED first, then GREEN.
///
/// Tests cover: passing, failing, error-setup, heavy-warnings, no-tests-exit5,
/// collect-only gate, malformed, non-UTF-8, stderr skip, never-worse,
/// field invariants (applied / reduction / recoverability / no-marker), and
/// full-pipeline marker boundary (Fix 1), verbose gate matrix (Fix 2),
/// and warnings Docs-footer exclusion (Fix 3).
use chrono::Utc;
use foldback_lib::argv::NormalizedCommand;
use foldback_lib::display::context::{ChannelContext, CommandContext};
use foldback_lib::display::outcome::{Recoverability, ReductionKind, SkipReason, ViewKind};
use foldback_lib::display::reducers::pytest::PytestReducer;
use foldback_lib::display::registry::{Reducer, Registry};
use foldback_lib::stash::Channel;

const REF_ID: &str = "abc123def456abc123def456abc123de";

fn expires() -> chrono::DateTime<Utc> {
    chrono::DateTime::<Utc>::MIN_UTC
}

fn make_cmd(args: &[&str]) -> CommandContext {
    CommandContext {
        command: "pytest".to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        normalized: NormalizedCommand::Pytest {
            module_invocation: false,
        },
        exit_code: 0,
        cwd: ".".to_string(),
    }
}

fn make_stdout_ctx<'a>(cmd: &'a CommandContext) -> ChannelContext<'a> {
    ChannelContext {
        command: cmd,
        channel: Channel::Stdout,
        ref_id: REF_ID,
        expires_at: &chrono::DateTime::<Utc>::MIN_UTC,
    }
}

fn make_stderr_ctx<'a>(cmd: &'a CommandContext) -> ChannelContext<'a> {
    ChannelContext {
        command: cmd,
        channel: Channel::Stderr,
        ref_id: REF_ID,
        expires_at: &chrono::DateTime::<Utc>::MIN_UTC,
    }
}

fn fixture(name: &str) -> Vec<u8> {
    let path = format!("tests/fixtures/pytest/{name}");
    std::fs::read(&path).unwrap_or_else(|e| panic!("fixture {path}: {e}"))
}

// ── Helper: assert reducer fields ────────────────────────────────────────────

fn assert_applied(display: &[u8]) {
    assert!(
        !display.windows(7).any(|w| w == b"[foldback"),
        "reducer must not inject foldback marker"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Gate tests — must return correct SkipReason WITHOUT reading body
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_gate_collect_only_long_flag() {
    let raw = fixture("collect_only.txt");
    let cmd = make_cmd(&["--collect-only"]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert_eq!(
        out.skip_reason,
        Some(SkipReason::MachineReadable),
        "--collect-only must return MachineReadable"
    );
    assert!(!out.applied, "gate must not apply");
}

#[test]
fn test_gate_collect_only_short_flag() {
    let raw = fixture("collect_only.txt");
    let cmd = make_cmd(&["--co"]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert_eq!(out.skip_reason, Some(SkipReason::MachineReadable));
    assert!(!out.applied);
}

#[test]
fn test_gate_json_report_flag() {
    let raw = fixture("passing_many.txt");
    let cmd = make_cmd(&["--json-report"]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert_eq!(out.skip_reason, Some(SkipReason::MachineReadable));
    assert!(!out.applied);
}

#[test]
fn test_gate_junitxml_flag() {
    let raw = fixture("passing_many.txt");
    let cmd = make_cmd(&["--junitxml=report.xml"]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert_eq!(out.skip_reason, Some(SkipReason::MachineReadable));
    assert!(!out.applied);
}

#[test]
fn test_gate_verbose_short_flag() {
    let raw = fixture("passing_many.txt");
    let cmd = make_cmd(&["-v"]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert_eq!(out.skip_reason, Some(SkipReason::MachineReadable));
    assert!(!out.applied);
}

#[test]
fn test_gate_verbose_long_flag() {
    let raw = fixture("passing_many.txt");
    let cmd = make_cmd(&["--verbose"]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert_eq!(out.skip_reason, Some(SkipReason::MachineReadable));
    assert!(!out.applied);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Channel routing — stderr must be skipped
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_stderr_channel_always_skipped() {
    let raw = fixture("passing_many.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stderr_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(
        !out.applied,
        "stderr must never be applied by pytest reducer"
    );
    assert!(
        out.skip_reason.is_some(),
        "stderr must produce a skip_reason"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Non-UTF-8 input
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_non_utf8_input_returns_non_utf8_skip() {
    // Binary input constructed inline — no file needed
    let raw: Vec<u8> = b"pytest output start\n"
        .iter()
        .copied()
        .chain([0xff, 0xfe, 0x80, 0x81])
        .chain(b"\nsome more\n".iter().copied())
        .collect();
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert_eq!(
        out.skip_reason,
        Some(SkipReason::NonUtf8),
        "non-UTF-8 input must return NonUtf8 skip"
    );
    assert!(!out.applied);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Malformed — no final summary → ParseFailed
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_malformed_no_final_summary_returns_parse_failed() {
    let raw = fixture("malformed.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert_eq!(
        out.skip_reason,
        Some(SkipReason::ParseFailed),
        "malformed output (no final summary) must return ParseFailed"
    );
    assert!(!out.applied);
}

// ═══════════════════════════════════════════════════════════════════════════════
// passing_many — success path
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_passing_many_applied_and_field_values() {
    let raw = fixture("passing_many.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied, "passing_many must be applied");
    assert!(out.skip_reason.is_none(), "must have no skip_reason");
    assert_eq!(
        out.view,
        ViewKind::PytestSummary,
        "view must be PytestSummary"
    );
    assert_eq!(
        out.reduction,
        ReductionKind::SemanticSummary,
        "reduction must be SemanticSummary"
    );
    assert_eq!(
        out.recoverability,
        Recoverability::Retrievable,
        "recoverability must be Retrievable"
    );
}

#[test]
fn test_passing_many_noise_removed() {
    let raw = fixture("passing_many.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied);
    let display = String::from_utf8(out.display.clone()).expect("display must be UTF-8");

    // Progress PASSED lines must be stripped
    assert!(
        !display.contains("PASSED"),
        "PASSED progress lines must be removed from display"
    );
    // Platform/header lines must be stripped
    assert!(
        !display.contains("platform linux"),
        "platform header must be removed"
    );
    assert!(
        !display.contains("rootdir:"),
        "rootdir header must be removed"
    );
    assert!(
        !display.contains("collected 120"),
        "collected-items line must be removed"
    );
}

#[test]
fn test_passing_many_final_summary_preserved() {
    let raw = fixture("passing_many.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied);
    let display = String::from_utf8(out.display.clone()).unwrap();

    // Final summary line must be present verbatim
    assert!(
        display.contains("120 passed"),
        "final summary must be preserved: '120 passed'"
    );
}

#[test]
fn test_passing_many_no_failed_injected() {
    // Success path must never inject FAILED text
    let raw = fixture("passing_many.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    if out.applied {
        let display = String::from_utf8(out.display.clone()).unwrap();
        assert!(
            !display.contains("FAILED"),
            "success path must not inject FAILED text"
        );
    }
}

#[test]
fn test_passing_many_no_marker_in_candidate() {
    let raw = fixture("passing_many.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    if out.applied {
        assert_applied(&out.display);
    }
}

#[test]
fn test_passing_many_never_worse() {
    let raw = fixture("passing_many.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    if out.applied {
        assert!(
            out.display.len() < raw.len(),
            "candidate must be strictly smaller than raw (never-worse): display={} raw={}",
            out.display.len(),
            raw.len()
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// failing_one — failure path
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_failing_one_applied() {
    let raw = fixture("failing_one.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied, "failing_one must be applied");
    assert!(out.skip_reason.is_none());
    assert_eq!(out.view, ViewKind::PytestSummary);
    assert_eq!(out.reduction, ReductionKind::SemanticSummary);
    assert_eq!(out.recoverability, Recoverability::Retrievable);
}

#[test]
fn test_failing_one_traceback_preserved() {
    let raw = fixture("failing_one.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied);
    let display = String::from_utf8(out.display.clone()).unwrap();

    // FAILURES section header must be preserved
    assert!(
        display.contains("FAILURES"),
        "FAILURES section must be preserved"
    );
    // Traceback content must be preserved
    assert!(
        display.contains("ZeroDivisionError"),
        "traceback ZeroDivisionError must be preserved"
    );
    // The test name in the failure block
    assert!(
        display.contains("test_divide_by_zero"),
        "failing test name must be preserved in traceback"
    );
}

#[test]
fn test_failing_one_short_summary_preserved() {
    let raw = fixture("failing_one.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied);
    let display = String::from_utf8(out.display.clone()).unwrap();

    assert!(
        display.contains("short test summary"),
        "short test summary section must be preserved"
    );
    assert!(
        display.contains("FAILED tests/test_unit.py::test_divide_by_zero"),
        "FAILED entry in short summary must be preserved"
    );
}

#[test]
fn test_failing_one_final_summary_preserved() {
    let raw = fixture("failing_one.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied);
    let display = String::from_utf8(out.display.clone()).unwrap();

    assert!(
        display.contains("1 failed"),
        "final summary '1 failed' must be preserved"
    );
    assert!(
        display.contains("100 passed"),
        "final summary '100 passed' must be preserved"
    );
}

#[test]
fn test_failing_one_progress_noise_removed() {
    let raw = fixture("failing_one.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied);
    let display = String::from_utf8(out.display.clone()).unwrap();

    // 100 PASSED progress lines must be stripped
    assert!(
        !display.contains("PASSED"),
        "PASSED progress lines must be removed"
    );
    // Header lines stripped
    assert!(!display.contains("platform linux"));
    assert!(!display.contains("collected 101"));
}

#[test]
fn test_failing_one_never_worse() {
    let raw = fixture("failing_one.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    if out.applied {
        assert!(
            out.display.len() < raw.len(),
            "display={} must be < raw={} (never-worse)",
            out.display.len(),
            raw.len()
        );
    }
}

#[test]
fn test_failing_one_no_marker() {
    let raw = fixture("failing_one.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    if out.applied {
        assert_applied(&out.display);
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// error_setup — ERRORS section
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_error_setup_applied() {
    let raw = fixture("error_setup.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied, "error_setup must be applied");
    assert!(out.skip_reason.is_none());
    assert_eq!(out.view, ViewKind::PytestSummary);
}

#[test]
fn test_error_setup_errors_section_preserved() {
    let raw = fixture("error_setup.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied);
    let display = String::from_utf8(out.display.clone()).unwrap();

    // ERRORS section must be kept
    assert!(
        display.contains("ERRORS"),
        "ERRORS section header must be preserved"
    );
    // Error content (ConnectionRefusedError)
    assert!(
        display.contains("ConnectionRefusedError"),
        "ConnectionRefusedError must be preserved in ERRORS section"
    );
}

#[test]
fn test_error_setup_final_summary_preserved() {
    let raw = fixture("error_setup.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied);
    let display = String::from_utf8(out.display.clone()).unwrap();

    assert!(
        display.contains("5 errors"),
        "final summary '5 errors' must be preserved"
    );
}

#[test]
fn test_error_setup_passing_noise_removed() {
    let raw = fixture("error_setup.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied);
    let display = String::from_utf8(out.display.clone()).unwrap();

    assert!(
        !display.contains("PASSED"),
        "PASSED progress noise must be removed"
    );
}

#[test]
fn test_error_setup_never_worse() {
    let raw = fixture("error_setup.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    if out.applied {
        assert!(out.display.len() < raw.len());
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// warnings_heavy — warning truncation
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_warnings_heavy_applied() {
    let raw = fixture("warnings_heavy.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied, "warnings_heavy must be applied");
    assert!(out.skip_reason.is_none());
    assert_eq!(out.view, ViewKind::PytestSummary);
}

#[test]
fn test_warnings_heavy_section_header_preserved() {
    let raw = fixture("warnings_heavy.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied);
    let display = String::from_utf8(out.display.clone()).unwrap();

    assert!(
        display.contains("warnings summary"),
        "warnings summary header must be preserved"
    );
}

#[test]
fn test_warnings_heavy_at_most_5_content_lines_plus_count() {
    let raw = fixture("warnings_heavy.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied);
    let display = String::from_utf8(out.display.clone()).unwrap();

    // The fixture has 12 warnings × 2 lines = 24 content lines + Docs line = 25 content lines.
    // We must NOT have all 25 content lines in display.
    // Count DeprecationWarning occurrences — should be at most 5 (since we cap at 5 lines)
    let deprecation_count = display.matches("DeprecationWarning").count();
    assert!(
        deprecation_count <= 5,
        "at most 5 DeprecationWarning lines must appear; found {deprecation_count}"
    );

    // There should be an omitted-count indicator (since 25 > 5 content lines omitted)
    assert!(
        display.contains("omitted") || display.contains("warning"),
        "must have omission indicator or warning count"
    );
}

#[test]
fn test_warnings_heavy_final_summary_preserved() {
    let raw = fixture("warnings_heavy.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied);
    let display = String::from_utf8(out.display.clone()).unwrap();

    assert!(
        display.contains("100 passed"),
        "final summary must be preserved"
    );
}

#[test]
fn test_warnings_heavy_never_worse() {
    let raw = fixture("warnings_heavy.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    if out.applied {
        assert!(
            out.display.len() < raw.len(),
            "warnings truncation must produce smaller output: display={} raw={}",
            out.display.len(),
            raw.len()
        );
    }
}

#[test]
fn test_warnings_heavy_no_marker() {
    let raw = fixture("warnings_heavy.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    if out.applied {
        assert_applied(&out.display);
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// no_tests_exit5 — key line preserved or graceful skip
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_no_tests_exit5_key_line_preserved_or_graceful_skip() {
    let raw = fixture("no_tests_exit5.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    if out.applied {
        let display = String::from_utf8(out.display.clone()).unwrap();
        assert!(
            display.contains("no tests ran"),
            "if applied, must preserve 'no tests ran'"
        );
        assert_applied(&out.display);
    } else {
        // Graceful skip is acceptable per spec: ParseFailed → generic/raw handles it
        assert!(
            out.skip_reason.is_some(),
            "skipped outcome must have a skip_reason"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Reducer trait contract
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_matches_pytest_normalized() {
    let r = PytestReducer;
    assert!(r.matches(&NormalizedCommand::Pytest {
        module_invocation: false
    }));
    assert!(r.matches(&NormalizedCommand::Pytest {
        module_invocation: true
    }));
    assert!(!r.matches(&NormalizedCommand::CargoTest));
    assert!(!r.matches(&NormalizedCommand::Generic));
}

#[test]
fn test_name_is_pytest() {
    assert_eq!(PytestReducer.name(), "pytest");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Empty input
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_empty_input_skipped() {
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&[], &ctx);

    assert!(!out.applied, "empty input must not be applied");
    assert!(
        matches!(
            out.skip_reason,
            Some(SkipReason::Empty) | Some(SkipReason::ParseFailed)
        ),
        "empty input must produce Empty or ParseFailed skip; got {:?}",
        out.skip_reason
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// expires_at placeholder (does not influence parse, only marker — pipeline adds marker)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_reducer_does_not_embed_expires_in_candidate() {
    // The reducer must NOT embed expires or ref_id in candidate display.
    // Those are added by the pipeline via build_specialized_marker.
    let raw = fixture("passing_many.txt");
    let exp = expires();
    let cmd = make_cmd(&[]);
    let ctx = ChannelContext {
        command: &cmd,
        channel: Channel::Stdout,
        ref_id: REF_ID,
        expires_at: &exp,
    };
    let out = PytestReducer.reduce(&raw, &ctx);

    if out.applied {
        let display = String::from_utf8(out.display.clone()).unwrap();
        assert!(
            !display.contains(REF_ID),
            "candidate must not embed ref_id (pipeline adds marker)"
        );
        assert!(
            !display.contains("expires"),
            "candidate must not embed expires (pipeline adds marker)"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Fix 1 (HIGH): Full-pipeline — specialized marker must appear on its own line.
//
// The candidate body must end with '\n' so the pipeline's marker append
// does not concatenate onto the final summary line.
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_pipeline_specialized_marker_on_own_line() {
    // Drive the full render_channel pipeline with default_registry so the
    // PytestReducer is called and the marker is appended by the pipeline.
    let raw = fixture("passing_many.txt");
    let exp = expires();
    let cmd = make_cmd(&[]);
    let ctx = ChannelContext {
        command: &cmd,
        channel: Channel::Stdout,
        ref_id: REF_ID,
        expires_at: &exp,
    };
    let registry = Registry::default_registry();
    let out = foldback_lib::display::render_channel(&raw, &ctx, &registry, true);

    assert!(out.applied, "passing_many must be applied by pipeline");

    let display_bytes = &out.display;
    let display_str = String::from_utf8(display_bytes.clone()).expect("display must be UTF-8");

    // Specialized marker must be present.
    assert!(
        display_str.contains("[foldback ref="),
        "specialized marker must be present in pipeline output"
    );
    assert!(
        display_str.contains("view=pytest"),
        "marker must contain view=pytest"
    );

    // The byte immediately before "[foldback ref=" must be '\n' (marker on own line).
    let marker_pos = display_str
        .find("[foldback ref=")
        .expect("marker must be present");
    assert!(marker_pos > 0, "marker must not be the very first byte");
    let byte_before_marker = display_bytes[marker_pos - 1];
    assert_eq!(
        byte_before_marker, b'\n',
        "marker must be preceded by '\\n' (own line); found {:?}",
        byte_before_marker as char
    );
}

#[test]
fn test_candidate_body_ends_with_newline() {
    // The reducer itself must return a candidate whose display ends with '\n'
    // so the pipeline-appended marker always starts on a fresh line.
    let raw = fixture("passing_many.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied, "must be applied");
    assert!(
        out.display.ends_with(b"\n"),
        "applied candidate display must end with '\\n' so pipeline marker is on its own line"
    );
}

#[test]
fn test_candidate_no_double_blank_line_at_end() {
    // Ensure adding the trailing newline does not introduce a double blank line.
    let raw = fixture("passing_many.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    if out.applied {
        assert!(
            !out.display.ends_with(b"\n\n"),
            "candidate must not end with double blank line"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Fix 2 (MEDIUM): Verbose gate must recognise -v, -vv, -vvv … (single-dash
// followed exclusively by one or more 'v' characters) but NOT -version.
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_gate_vv_is_machine_readable() {
    let raw = fixture("passing_many.txt");
    let cmd = make_cmd(&["-vv"]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);
    assert_eq!(
        out.skip_reason,
        Some(SkipReason::MachineReadable),
        "-vv must trigger MachineReadable gate"
    );
    assert!(!out.applied);
}

#[test]
fn test_gate_vvv_is_machine_readable() {
    let raw = fixture("passing_many.txt");
    let cmd = make_cmd(&["-vvv"]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);
    assert_eq!(
        out.skip_reason,
        Some(SkipReason::MachineReadable),
        "-vvv must trigger MachineReadable gate"
    );
    assert!(!out.applied);
}

#[test]
fn test_gate_version_flag_not_machine_readable() {
    // -version has mixed characters after the dash — must NOT gate.
    let raw = fixture("passing_many.txt");
    let cmd = make_cmd(&["-version"]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);
    assert_ne!(
        out.skip_reason,
        Some(SkipReason::MachineReadable),
        "-version must NOT be treated as a verbose flag"
    );
}

#[test]
fn test_gate_v_combined_with_other_flags_still_gates() {
    // -vv alongside other non-gating flags must still trigger.
    let raw = fixture("passing_many.txt");
    let cmd = make_cmd(&["-q", "-vv", "tests/"]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);
    assert_eq!(
        out.skip_reason,
        Some(SkipReason::MachineReadable),
        "-vv in arg list must still gate even with other args"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Fix 3 (MEDIUM): '-- Docs: ...' footer must NOT be counted as a warning line.
//
// When all actual warnings fit in MAX_WARNING_LINES (5), the omitted count
// must be 0 and the Docs footer must not appear in the display.
// ═══════════════════════════════════════════════════════════════════════════════

/// Build inline pytest stdout with exactly `warning_count` DeprecationWarning
/// lines in the warnings section, plus the standard Docs footer.
fn build_pytest_with_n_warnings(warning_count: usize) -> String {
    let total_tests = 60usize;
    let mut text = String::new();
    text.push_str(
        "============================= test session starts ==============================\n",
    );
    text.push_str("platform linux -- Python 3.11.0, pytest-7.4.0, pluggy-1.3.0\n");
    text.push_str("rootdir: /home/user/project\n");
    text.push_str("collected 60 items\n");
    text.push('\n');
    for i in 1..=total_tests {
        let pct = i * 100 / total_tests;
        text.push_str(&format!(
            "tests/test_m.py::test_fn_{i:03} PASSED                       [{pct:3}%]\n"
        ));
    }
    text.push('\n');
    text.push_str(
        "============================= warnings summary ==============================\n",
    );
    for i in 1..=warning_count {
        text.push_str(&format!(
            "  /path/to/lib.py:{}: DeprecationWarning: api_v{i} is deprecated, use api_v{} instead\n",
            i * 10,
            i + 1
        ));
    }
    // Standard pytest Docs footer — must NOT count as a warning.
    text.push_str("-- Docs: https://docs.pytest.org/en/stable/how-to/capture-warnings.html\n");
    text.push_str(
        "============================== 60 passed in 1.23s ==============================\n",
    );
    text
}

#[test]
fn test_warnings_docs_footer_not_counted_when_all_warnings_fit() {
    // Exactly 5 DeprecationWarning lines (= MAX_WARNING_LINES) + Docs footer.
    // All 5 warnings must appear; NO "(N warnings omitted)" line.
    let text = build_pytest_with_n_warnings(5);
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(text.as_bytes(), &ctx);

    assert!(out.applied, "must be applied");
    let display = String::from_utf8(out.display.clone()).unwrap();

    // All 5 DeprecationWarning lines must appear (none omitted).
    let dw_count = display.matches("DeprecationWarning").count();
    assert_eq!(
        dw_count, 5,
        "all 5 DeprecationWarning lines must be shown; got {dw_count}"
    );

    // No omission message (Docs footer must not push count over limit).
    assert!(
        !display.contains("warnings omitted"),
        "must NOT show omission message; Docs footer must not be counted"
    );

    // Docs footer must not appear in display at all.
    assert!(
        !display.contains("-- Docs:"),
        "-- Docs: footer must be excluded from display"
    );
}

#[test]
fn test_warnings_docs_footer_excluded_from_omitted_count() {
    // 6 DeprecationWarning lines (one over limit) + Docs footer.
    // Expected: 5 shown, omitted = 1 (not 2: Docs must not be counted).
    let text = build_pytest_with_n_warnings(6);
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(text.as_bytes(), &ctx);

    assert!(out.applied, "must be applied");
    let display = String::from_utf8(out.display.clone()).unwrap();

    // Exactly 5 DeprecationWarning lines must appear.
    let dw_count = display.matches("DeprecationWarning").count();
    assert_eq!(
        dw_count, 5,
        "exactly 5 DeprecationWarning lines must be shown; got {dw_count}"
    );

    // Must show "(1 warnings omitted)", not "(2 warnings omitted)".
    assert!(
        display.contains("(1 warnings omitted)"),
        "omitted count must be 1 (the 6th warning), not 2 (Docs footer excluded); display:\n{display}"
    );

    // Docs footer must not appear.
    assert!(
        !display.contains("-- Docs:"),
        "-- Docs: footer must not appear"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// pytest 8.4.2 quiet-mode (-q): bare final summary (no = borders)
//
// Real pytest --quiet output ends with a bare line like:
//   `5000 passed in 12.34s`   or   `1 failed, 4950 passed in 15.43s`
// The current `is_final_summary` only accepts = bordered lines, causing ParseFailed.
// These tests are RED until the bare-summary grammar is added.
// ═══════════════════════════════════════════════════════════════════════════════

// ── Positive: quiet passing fixture ──────────────────────────────────────────

#[test]
fn test_quiet_passing_many_reducer_applied() {
    // RED: stub/current impl returns ParseFailed for bare summary
    let raw = fixture("quiet_passing_many.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(
        out.applied,
        "quiet_passing_many must be applied (bare '5000 passed in 12.34s' must be recognized)"
    );
    assert!(out.skip_reason.is_none());
    assert_eq!(out.view, ViewKind::PytestSummary);
    assert_eq!(out.reduction, ReductionKind::SemanticSummary);
    assert_eq!(out.recoverability, Recoverability::Retrievable);
}

#[test]
fn test_quiet_passing_many_dots_removed() {
    let raw = fixture("quiet_passing_many.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied);
    let display = String::from_utf8(out.display.clone()).unwrap();

    // Progress dots must be stripped
    assert!(
        !display.contains(".................................................."),
        "dot progress lines must be removed"
    );
    // Platform header must be stripped
    assert!(
        !display.contains("platform linux"),
        "platform header must be removed"
    );
}

#[test]
fn test_quiet_passing_many_bare_summary_preserved_verbatim() {
    // The bare summary line must be kept VERBATIM from raw.
    let raw = fixture("quiet_passing_many.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied);
    let display = String::from_utf8(out.display.clone()).unwrap();

    // The exact bare summary string from the fixture must appear.
    assert!(
        display.contains("5000 passed in 12.34s"),
        "bare final summary must be preserved verbatim; got:\n{display}"
    );
    // Must NOT contain synthesized/modified counts.
    assert!(
        !display.contains("FAILED"),
        "success path must not inject FAILED text"
    );
}

#[test]
fn test_quiet_passing_many_no_marker_in_candidate() {
    let raw = fixture("quiet_passing_many.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    if out.applied {
        assert!(
            !out.display.windows(7).any(|w| w == b"[foldback"),
            "reducer must not inject foldback marker"
        );
    }
}

#[test]
fn test_quiet_passing_many_candidate_ends_with_newline() {
    let raw = fixture("quiet_passing_many.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied);
    assert!(
        out.display.ends_with(b"\n"),
        "candidate must end with '\\n' so pipeline marker is on its own line"
    );
}

#[test]
fn test_quiet_passing_many_never_worse() {
    let raw = fixture("quiet_passing_many.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    if out.applied {
        assert!(
            out.display.len() < raw.len(),
            "candidate display={} must be < raw={} (never-worse)",
            out.display.len(),
            raw.len()
        );
    }
}

// ── Positive: quiet failing fixture ──────────────────────────────────────────

#[test]
fn test_quiet_failing_one_reducer_applied() {
    let raw = fixture("quiet_failing_one.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(
        out.applied,
        "quiet_failing_one must be applied (bare '1 failed, 4950 passed in 15.43s' must be recognized)"
    );
    assert!(out.skip_reason.is_none());
    assert_eq!(out.view, ViewKind::PytestSummary);
}

#[test]
fn test_quiet_failing_one_failures_section_preserved() {
    let raw = fixture("quiet_failing_one.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied);
    let display = String::from_utf8(out.display.clone()).unwrap();

    assert!(
        display.contains("FAILURES"),
        "FAILURES section must be preserved"
    );
    assert!(
        display.contains("ZeroDivisionError"),
        "traceback content must be preserved"
    );
}

#[test]
fn test_quiet_failing_one_short_summary_preserved() {
    let raw = fixture("quiet_failing_one.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied);
    let display = String::from_utf8(out.display.clone()).unwrap();

    assert!(
        display.contains("short test summary"),
        "short test summary section must be preserved"
    );
    assert!(
        display.contains("FAILED tests/test_unit.py::test_divide_by_zero"),
        "FAILED item must be preserved in short summary"
    );
}

#[test]
fn test_quiet_failing_one_bare_summary_preserved_verbatim() {
    let raw = fixture("quiet_failing_one.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied);
    let display = String::from_utf8(out.display.clone()).unwrap();

    assert!(
        display.contains("1 failed, 4950 passed in 15.43s"),
        "bare final summary must be preserved verbatim; got:\n{display}"
    );
}

#[test]
fn test_quiet_failing_one_dots_removed() {
    let raw = fixture("quiet_failing_one.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    assert!(out.applied);
    let display = String::from_utf8(out.display.clone()).unwrap();

    // Progress dot lines must be removed (they are many)
    let dot_lines = display
        .lines()
        .filter(|l| l.trim_start().starts_with('.') && l.contains('%'))
        .count();
    assert_eq!(
        dot_lines, 0,
        "dot progress lines must be removed from display"
    );
}

#[test]
fn test_quiet_failing_one_never_worse() {
    let raw = fixture("quiet_failing_one.txt");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);

    if out.applied {
        assert!(out.display.len() < raw.len());
    }
}

// ── Pipeline: quiet fixture → view=pytest marker on own line ─────────────────

#[test]
fn test_quiet_pipeline_produces_pytest_marker() {
    let raw = fixture("quiet_passing_many.txt");
    let exp = expires();
    let cmd = make_cmd(&[]);
    let ctx = ChannelContext {
        command: &cmd,
        channel: Channel::Stdout,
        ref_id: REF_ID,
        expires_at: &exp,
    };
    let registry = Registry::default_registry();
    let out = foldback_lib::display::render_channel(&raw, &ctx, &registry, true);

    assert!(
        out.applied,
        "pipeline must apply reducer to quiet_passing_many"
    );
    assert_eq!(
        out.view,
        ViewKind::PytestSummary,
        "must produce PytestSummary view"
    );

    let display_str = String::from_utf8(out.display.clone()).expect("display must be UTF-8");
    assert!(
        display_str.contains("view=pytest"),
        "pipeline display must contain view=pytest marker field"
    );
    assert!(
        display_str.contains("recoverability=retrievable"),
        "pipeline display must contain recoverability=retrievable"
    );

    // Marker must be on its own line.
    let marker_pos = display_str
        .find("[foldback ref=")
        .expect("specialized marker must be present");
    assert!(marker_pos > 0, "marker must not be at position 0");
    assert_eq!(
        out.display[marker_pos - 1],
        b'\n',
        "byte before marker must be '\\n' (marker on own line)"
    );
}

#[test]
fn test_quiet_pipeline_strictly_beneficial() {
    let raw = fixture("quiet_passing_many.txt");
    let exp = expires();
    let cmd = make_cmd(&[]);
    let ctx = ChannelContext {
        command: &cmd,
        channel: Channel::Stdout,
        ref_id: REF_ID,
        expires_at: &exp,
    };
    let registry = Registry::default_registry();
    let out = foldback_lib::display::render_channel(&raw, &ctx, &registry, true);

    assert!(
        out.display.len() < raw.len(),
        "pipeline output must be strictly smaller than raw (never-worse with marker); display={} raw={}",
        out.display.len(),
        raw.len()
    );
}

// ── Negative: bare-summary grammar rejects invalid lines ─────────────────────
//
// Tests use inline text so the ONLY possible summary line is the bad one.
// Outcome: ParseFailed (no valid final summary found).

fn build_pytest_output_with_last_line(last_line: &str) -> Vec<u8> {
    // Enough header+progress content to exceed threshold, then the "summary" line.
    let mut text = String::new();
    text.push_str(
        "============================= test session starts ==============================\n",
    );
    text.push_str("platform linux -- Python 3.11.0\n");
    text.push_str("rootdir: /home/user/project\n");
    text.push_str("collected 200 items\n\n");
    for i in 1..=200 {
        let pct = i * 100 / 200;
        text.push_str(&format!(
            "tests/t.py::test_{i:03} PASSED                    [{pct:3}%]\n"
        ));
    }
    text.push_str(last_line);
    text.push('\n');
    text.into_bytes()
}

#[test]
fn test_bare_summary_negative_no_duration() {
    // "1 passed" — missing " in <duration>" → must NOT match → ParseFailed
    let raw = build_pytest_output_with_last_line("1 passed");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);
    assert_eq!(
        out.skip_reason,
        Some(SkipReason::ParseFailed),
        "'1 passed' (no duration) must cause ParseFailed; got {:?}",
        out.skip_reason
    );
}

#[test]
fn test_bare_summary_negative_unknown_status() {
    // "5 requests handled in 0.1s" — "requests handled" not a pytest status word
    let raw = build_pytest_output_with_last_line("5 requests handled in 0.1s");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);
    assert_eq!(
        out.skip_reason,
        Some(SkipReason::ParseFailed),
        "unknown status must cause ParseFailed"
    );
}

#[test]
fn test_bare_summary_negative_extra_prefix() {
    // "[LOG] 1 passed in 0.12s" — extra prefix before digit → reject
    let raw = build_pytest_output_with_last_line("[LOG] 1 passed in 0.12s");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);
    assert_eq!(
        out.skip_reason,
        Some(SkipReason::ParseFailed),
        "line with extra prefix must not match bare summary"
    );
}

#[test]
fn test_bare_summary_negative_assert_line() {
    // `assert result == "1 passed in 0.1s"` — starts with 'a' not digit → reject
    let raw = build_pytest_output_with_last_line("assert result == \"1 passed in 0.1s\"");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);
    assert_eq!(
        out.skip_reason,
        Some(SkipReason::ParseFailed),
        "assert line must not match bare summary"
    );
}

#[test]
fn test_bare_summary_negative_malformed_count() {
    // "abc passed in 0.1s" — count is not a non-negative integer → reject
    let raw = build_pytest_output_with_last_line("abc passed in 0.1s");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);
    assert_eq!(
        out.skip_reason,
        Some(SkipReason::ParseFailed),
        "non-integer count must cause ParseFailed"
    );
}

// ── Duration digit gap fix (LOW): "ms" / "hs" must not be valid durations ───
//
// looks_like_duration("ms") was true before the fix because 'm' is in the
// character allowlist and the length-2 guard passed.  The full-pipeline test
// below proves that "10 passed in ms" stays ParseFailed after the fix.

#[test]
fn test_bare_summary_negative_duration_ms() {
    // "10 passed in ms" — "ms" has no digit → must not match → ParseFailed
    let raw = build_pytest_output_with_last_line("10 passed in ms");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);
    assert_eq!(
        out.skip_reason,
        Some(SkipReason::ParseFailed),
        "'10 passed in ms' must not be accepted as a final summary (no digit in duration)"
    );
}

#[test]
fn test_bare_summary_negative_duration_hs() {
    let raw = build_pytest_output_with_last_line("5 passed in hs");
    let cmd = make_cmd(&[]);
    let ctx = make_stdout_ctx(&cmd);
    let out = PytestReducer.reduce(&raw, &ctx);
    assert_eq!(
        out.skip_reason,
        Some(SkipReason::ParseFailed),
        "'5 passed in hs' must not be accepted as a final summary"
    );
}
