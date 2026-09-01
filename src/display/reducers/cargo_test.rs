/// Cargo-test-specific reducer.
///
/// Phase 2 — pure-function reducer for `cargo test` stdout.
///
/// ## Passthrough gates (Skipped before any parsing):
/// - `--message-format` or `--message-format=*` present in args → `MachineReadable`
/// - Non-UTF-8 stdout → `NonUtf8`
/// - Empty stdout → `Empty`
/// - `Channel::Stderr` → `ParseFailed` (compile diagnostics stay generic/raw)
///
/// ## Candidate body (stdout only):
/// - `running N tests` / `running 1 test` header lines — kept
/// - `test <name> ... ok` / `test <name> ... ignored` progress lines — **dropped** (noise)
/// - `test <name> ... FAILED` lines — kept
/// - `failures:` section through `test result:` (blocks + names list) — kept verbatim
/// - Each `test result:` final summary line — kept
/// - No `test result:` found in output → `ParseFailed`
///
/// The candidate is returned **without** the foldback marker; the pipeline appends it.
use crate::argv::NormalizedCommand;
use crate::display::context::ChannelContext;
use crate::display::outcome::{Recoverability, ReduceOutcome, ReductionKind, SkipReason, ViewKind};
use crate::display::registry::Reducer;
use crate::stash::Channel;

pub struct CargoTestReducer;

impl Reducer for CargoTestReducer {
    fn name(&self) -> &'static str {
        "cargo-test"
    }

    fn matches(&self, norm: &NormalizedCommand) -> bool {
        matches!(norm, NormalizedCommand::CargoTest)
    }

    fn reduce(&self, input: &[u8], ctx: &ChannelContext) -> ReduceOutcome {
        // Stderr: compile diagnostics are conservative generic/raw.
        if ctx.channel == Channel::Stderr {
            return ReduceOutcome::skipped(SkipReason::ParseFailed);
        }

        // Empty stdout: nothing to reduce.
        if input.is_empty() {
            return ReduceOutcome::skipped(SkipReason::Empty);
        }

        // Gate: machine-readable format flag in args.
        if has_message_format_arg(&ctx.command.args) {
            return ReduceOutcome::skipped(SkipReason::MachineReadable);
        }

        // Non-UTF-8: fall through to generic/raw.
        let text = match std::str::from_utf8(input) {
            Ok(t) => t,
            Err(_) => return ReduceOutcome::skipped(SkipReason::NonUtf8),
        };

        match reduce_cargo_stdout(text) {
            Some(candidate) => ReduceOutcome {
                display: candidate,
                applied: true,
                view: ViewKind::CargoTestSummary,
                reduction: ReductionKind::SemanticSummary,
                recoverability: Recoverability::Retrievable,
                skip_reason: None,
            },
            None => ReduceOutcome::skipped(SkipReason::ParseFailed),
        }
    }
}

/// Return `true` if any arg equals `--message-format` or starts with `--message-format=`.
fn has_message_format_arg(args: &[String]) -> bool {
    args.iter()
        .any(|a| a == "--message-format" || a.starts_with("--message-format="))
}

/// Produce the reduced candidate body for `cargo test` stdout.
///
/// Returns `None` when no `test result:` line is found (triggering `ParseFailed`).
/// Returns `Some(bytes)` with ok/ignored progress lines dropped and all
/// failure sections + summaries preserved verbatim.
fn reduce_cargo_stdout(text: &str) -> Option<Vec<u8>> {
    let lines: Vec<&str> = text.lines().collect();

    // Require at least one `test result:` line; without it we cannot confirm any
    // test actually ran or that we understand this output format.
    if !lines.iter().any(|l| l.starts_with("test result:")) {
        return None;
    }

    let mut output: Vec<&str> = Vec::with_capacity(lines.len());
    // True while inside a `failures:` section (from first `failures:` to `test result:`).
    let mut in_failure_section = false;

    for line in &lines {
        // Each binary's final summary: always keep; reset failure-section state.
        if line.starts_with("test result:") {
            output.push(line);
            in_failure_section = false;
            continue;
        }

        // `running N tests` / `running 1 test` header: always keep.
        if is_running_header(line) {
            in_failure_section = false;
            output.push(line);
            continue;
        }

        // `failures:` marks the start of the failure section.
        if *line == "failures:" {
            in_failure_section = true;
            output.push(line);
            continue;
        }

        // Inside failure section: keep everything verbatim (blocks + names list).
        if in_failure_section {
            output.push(line);
            continue;
        }

        // Progress lines to drop: `test <name> ... ok` and `test <name> ... ignored`.
        if is_test_progress_skip(line) {
            continue;
        }

        // Everything else (FAILED lines, blank lines, other lines): keep.
        output.push(line);
    }

    let mut result = output.join("\n");
    if text.ends_with('\n') {
        result.push('\n');
    }

    Some(result.into_bytes())
}

/// True for `running N tests` or `running 1 test`.
fn is_running_header(line: &str) -> bool {
    line.starts_with("running ") && (line.ends_with(" tests") || line.ends_with(" test"))
}

/// True for progress lines that should be dropped: `test ... ok` / `test ... ignored`.
fn is_test_progress_skip(line: &str) -> bool {
    if !line.starts_with("test ") {
        return false;
    }
    line.ends_with(" ... ok") || line.ends_with(" ... ignored")
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::argv::NormalizedCommand;
    use crate::display::context::{ChannelContext, CommandContext};
    use crate::display::outcome::{Recoverability, ReductionKind, SkipReason, ViewKind};

    // ── Fixtures ──────────────────────────────────────────────────────────────

    const PASSING_MANY: &[u8] =
        include_bytes!("../../../tests/fixtures/cargo-test/passing_many.txt");
    const FAILING_ONE: &[u8] = include_bytes!("../../../tests/fixtures/cargo-test/failing_one.txt");
    const MULTIPLE_BINARIES: &[u8] =
        include_bytes!("../../../tests/fixtures/cargo-test/multiple_binaries.txt");
    const IGNORED_FILTERED: &[u8] =
        include_bytes!("../../../tests/fixtures/cargo-test/ignored_filtered.txt");
    const COMPILE_FAILURE_EMPTY: &[u8] =
        include_bytes!("../../../tests/fixtures/cargo-test/compile_failure_stdout_empty.txt");
    const MESSAGE_FORMAT_JSON: &[u8] =
        include_bytes!("../../../tests/fixtures/cargo-test/message_format_json.txt");
    const MALFORMED: &[u8] = include_bytes!("../../../tests/fixtures/cargo-test/malformed.txt");
    const MIXED_MULTI_BINARY: &[u8] =
        include_bytes!("../../../tests/fixtures/cargo-test/mixed_multi_binary.txt");

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_cmd(args: Vec<&str>) -> CommandContext {
        CommandContext {
            command: "cargo".to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            normalized: NormalizedCommand::CargoTest,
            exit_code: 0,
            cwd: ".".to_string(),
        }
    }

    fn stdout_ctx<'a>(cmd: &'a CommandContext) -> ChannelContext<'a> {
        ChannelContext {
            command: cmd,
            channel: Channel::Stdout,
            ref_id: "abc123def456abc123def456abc123de",
            expires_at: &chrono::DateTime::<chrono::Utc>::MIN_UTC,
        }
    }

    fn stderr_ctx<'a>(cmd: &'a CommandContext) -> ChannelContext<'a> {
        ChannelContext {
            command: cmd,
            channel: Channel::Stderr,
            ref_id: "abc123def456abc123def456abc123de",
            expires_at: &chrono::DateTime::<chrono::Utc>::MIN_UTC,
        }
    }

    fn display_str(outcome: &ReduceOutcome) -> String {
        String::from_utf8_lossy(&outcome.display).into_owned()
    }

    // ── Existing stub contract tests (must stay green) ────────────────────────

    #[test]
    fn test_cargo_test_reducer_matches_cargo_test_normalized() {
        let r = CargoTestReducer;
        assert!(r.matches(&NormalizedCommand::CargoTest));
    }

    #[test]
    fn test_cargo_test_reducer_does_not_match_other_commands() {
        let r = CargoTestReducer;
        assert!(!r.matches(&NormalizedCommand::Pytest {
            module_invocation: false
        }));
        assert!(!r.matches(&NormalizedCommand::Generic));
    }

    #[test]
    fn test_unparseable_input_without_test_result_line_returns_parse_failed() {
        // Input that looks like cargo output but has no `test result:` → ParseFailed.
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let outcome = CargoTestReducer.reduce(b"some cargo test output", &ctx);
        assert!(outcome.skip_reason.is_some(), "must skip unparseable input");
        assert!(!outcome.applied, "must not apply on unparseable input");
    }

    // ── Gate: --message-format ────────────────────────────────────────────────

    #[test]
    fn test_gate_message_format_flag_separate() {
        let cmd = make_cmd(vec!["test", "--message-format", "json"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(PASSING_MANY, &ctx);
        assert_eq!(
            out.skip_reason,
            Some(SkipReason::MachineReadable),
            "--message-format as separate flag must gate"
        );
        assert!(!out.applied);
    }

    #[test]
    fn test_gate_message_format_eq_form() {
        let cmd = make_cmd(vec![
            "test",
            "--message-format=json-diagnostic-rendered-ansi",
        ]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(PASSING_MANY, &ctx);
        assert_eq!(
            out.skip_reason,
            Some(SkipReason::MachineReadable),
            "--message-format=* must gate"
        );
        assert!(!out.applied);
    }

    // ── Gate: stderr channel ──────────────────────────────────────────────────

    #[test]
    fn test_gate_stderr_channel_skipped() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stderr_ctx(&cmd);
        let out = CargoTestReducer.reduce(PASSING_MANY, &ctx);
        assert!(
            out.skip_reason.is_some(),
            "stderr must be skipped (compile diagnostics stay generic/raw)"
        );
        assert!(!out.applied);
    }

    // ── Gate: empty input ─────────────────────────────────────────────────────

    #[test]
    fn test_gate_empty_input_skipped() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(COMPILE_FAILURE_EMPTY, &ctx);
        assert_eq!(
            out.skip_reason,
            Some(SkipReason::Empty),
            "empty stdout must skip with Empty"
        );
        assert!(!out.applied);
    }

    // ── Gate: non-UTF-8 ───────────────────────────────────────────────────────

    #[test]
    fn test_gate_non_utf8_skipped() {
        // Construct bytes that are invalid UTF-8.
        let invalid: Vec<u8> = b"running 2 tests\n\xFF\xFE\n".to_vec();
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(&invalid, &ctx);
        assert_eq!(
            out.skip_reason,
            Some(SkipReason::NonUtf8),
            "non-UTF-8 input must skip with NonUtf8"
        );
        assert!(!out.applied);
    }

    // ── Gate: malformed / no test result ─────────────────────────────────────

    #[test]
    fn test_malformed_no_test_result_parse_failed() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(MALFORMED, &ctx);
        assert_eq!(
            out.skip_reason,
            Some(SkipReason::ParseFailed),
            "output without `test result:` must return ParseFailed"
        );
        assert!(!out.applied);
    }

    // ── Gate: JSON format fixture (no test result: line) ──────────────────────

    #[test]
    fn test_message_format_json_fixture_no_test_result_parse_failed() {
        // Without --message-format in args, JSON output has no `test result:` → ParseFailed.
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(MESSAGE_FORMAT_JSON, &ctx);
        assert_eq!(
            out.skip_reason,
            Some(SkipReason::ParseFailed),
            "JSON format output without --message-format flag still has no test result: → ParseFailed"
        );
        assert!(!out.applied);
    }

    // ── passing_many fixture ──────────────────────────────────────────────────

    #[test]
    fn test_passing_many_applied_true() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(PASSING_MANY, &ctx);
        assert!(out.applied, "passing_many must be applied (reduced)");
        assert!(out.skip_reason.is_none());
    }

    #[test]
    fn test_passing_many_ok_lines_removed() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(PASSING_MANY, &ctx);
        let s = display_str(&out);
        assert!(
            !s.contains("... ok"),
            "passing_many candidate must not contain `... ok` progress lines"
        );
    }

    #[test]
    fn test_passing_many_keeps_running_header() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(PASSING_MANY, &ctx);
        let s = display_str(&out);
        assert!(
            s.contains("running 20 tests"),
            "candidate must preserve `running N tests` header"
        );
    }

    #[test]
    fn test_passing_many_keeps_summary() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(PASSING_MANY, &ctx);
        let s = display_str(&out);
        assert!(
            s.contains("test result: ok. 20 passed"),
            "candidate must preserve `test result:` final summary"
        );
    }

    #[test]
    fn test_passing_many_never_worse() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(PASSING_MANY, &ctx);
        assert!(
            out.display.len() < PASSING_MANY.len(),
            "candidate ({} bytes) must be strictly smaller than raw ({} bytes)",
            out.display.len(),
            PASSING_MANY.len()
        );
    }

    #[test]
    fn test_passing_many_no_marker_in_candidate() {
        // The pipeline appends the marker; the reducer must not include it.
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(PASSING_MANY, &ctx);
        let s = display_str(&out);
        assert!(
            !s.contains("[foldback"),
            "candidate must not contain the foldback marker (pipeline appends it)"
        );
    }

    // ── view / metadata ───────────────────────────────────────────────────────

    #[test]
    fn test_view_kind_is_cargo_test_summary() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(PASSING_MANY, &ctx);
        assert_eq!(
            out.view,
            ViewKind::CargoTestSummary,
            "view must be CargoTestSummary"
        );
    }

    #[test]
    fn test_reduction_is_semantic_summary() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(PASSING_MANY, &ctx);
        assert_eq!(
            out.reduction,
            ReductionKind::SemanticSummary,
            "reduction must be SemanticSummary"
        );
    }

    #[test]
    fn test_recoverability_is_retrievable() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(PASSING_MANY, &ctx);
        assert_eq!(
            out.recoverability,
            Recoverability::Retrievable,
            "recoverability must be Retrievable"
        );
    }

    // ── failing_one fixture ───────────────────────────────────────────────────

    #[test]
    fn test_failing_one_applied_true() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(FAILING_ONE, &ctx);
        assert!(out.applied, "failing_one must be applied");
        assert!(out.skip_reason.is_none());
    }

    #[test]
    fn test_failing_one_keeps_failure_block() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(FAILING_ONE, &ctx);
        let s = display_str(&out);
        assert!(
            s.contains("---- tests::the_failure stdout ----"),
            "failure block header must be preserved"
        );
        assert!(
            s.contains("assertion `left == right` failed"),
            "failure block content must be preserved"
        );
    }

    #[test]
    fn test_failing_one_keeps_failures_names_list() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(FAILING_ONE, &ctx);
        let s = display_str(&out);
        assert!(
            s.contains("failures:"),
            "failures: section must be preserved"
        );
        assert!(
            s.contains("tests::the_failure"),
            "failed test name must appear in failures list"
        );
    }

    #[test]
    fn test_failing_one_keeps_failed_test_line() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(FAILING_ONE, &ctx);
        let s = display_str(&out);
        assert!(
            s.contains("test tests::the_failure ... FAILED"),
            "FAILED test progress line must be preserved"
        );
    }

    #[test]
    fn test_failing_one_keeps_summary() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(FAILING_ONE, &ctx);
        let s = display_str(&out);
        assert!(
            s.contains("test result: FAILED. 4 passed; 1 failed"),
            "FAILED summary line must be preserved verbatim"
        );
    }

    #[test]
    fn test_failing_one_ok_lines_removed() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(FAILING_ONE, &ctx);
        let s = display_str(&out);
        // The 4 passing tests must not appear as `... ok` lines.
        assert!(
            !s.contains("tests::compute_sum ... ok"),
            "`... ok` progress lines must be removed"
        );
        assert!(
            !s.contains("tests::compute_diff ... ok"),
            "`... ok` progress lines must be removed"
        );
    }

    #[test]
    fn test_failing_one_no_fabricated_success() {
        // The summary must say FAILED, not ok — never disguise a failure.
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(FAILING_ONE, &ctx);
        let s = display_str(&out);
        assert!(
            !s.contains("test result: ok"),
            "must never fabricate a passing summary when tests actually failed"
        );
    }

    // ── multiple_binaries fixture ─────────────────────────────────────────────

    #[test]
    fn test_multiple_binaries_keeps_all_summaries() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(MULTIPLE_BINARIES, &ctx);
        let s = display_str(&out);
        let count = s.matches("test result:").count();
        assert_eq!(
            count, 2,
            "both binary summaries must be preserved (found {count})"
        );
    }

    #[test]
    fn test_multiple_binaries_ok_lines_removed() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(MULTIPLE_BINARIES, &ctx);
        let s = display_str(&out);
        assert!(
            !s.contains("... ok"),
            "ok progress lines must be removed from multiple-binary output"
        );
    }

    // ── ignored_filtered fixture ──────────────────────────────────────────────

    #[test]
    fn test_ignored_filtered_applied_true() {
        // 0 tests run (all filtered) but there IS a real `test result:` line.
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(IGNORED_FILTERED, &ctx);
        assert!(
            out.applied,
            "ignored/filtered output with real summary must be applied"
        );
        assert!(out.skip_reason.is_none());
    }

    #[test]
    fn test_ignored_filtered_keeps_summary() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(IGNORED_FILTERED, &ctx);
        let s = display_str(&out);
        assert!(
            s.contains("test result: ok. 0 passed; 0 failed; 2 ignored"),
            "real summary with ignored/filtered count must be preserved"
        );
    }

    #[test]
    fn test_ignored_filtered_keeps_running_header() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(IGNORED_FILTERED, &ctx);
        let s = display_str(&out);
        assert!(
            s.contains("running 0 tests"),
            "`running 0 tests` header must be preserved"
        );
    }

    // ── mixed_multi_binary fixture ────────────────────────────────────────────
    // Covers the case where one binary passes and another fails: the candidate
    // must preserve BOTH `test result:` summaries verbatim and must not
    // misrepresent the failed binary as successful.

    #[test]
    fn test_mixed_multi_binary_applied() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(MIXED_MULTI_BINARY, &ctx);
        assert!(out.applied, "mixed multi-binary output must be applied");
        assert!(out.skip_reason.is_none());
    }

    #[test]
    fn test_mixed_multi_binary_keeps_both_summaries() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(MIXED_MULTI_BINARY, &ctx);
        let s = display_str(&out);
        let count = s.matches("test result:").count();
        assert_eq!(
            count, 2,
            "both binary summaries must appear — found {count} `test result:` lines"
        );
    }

    #[test]
    fn test_mixed_multi_binary_ok_summary_preserved() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(MIXED_MULTI_BINARY, &ctx);
        let s = display_str(&out);
        assert!(
            s.contains("test result: ok."),
            "the passing binary summary must be preserved"
        );
    }

    #[test]
    fn test_mixed_multi_binary_failed_summary_preserved() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(MIXED_MULTI_BINARY, &ctx);
        let s = display_str(&out);
        assert!(
            s.contains("test result: FAILED."),
            "the failing binary summary must be preserved verbatim"
        );
    }

    #[test]
    fn test_mixed_multi_binary_failed_test_line_preserved() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(MIXED_MULTI_BINARY, &ctx);
        let s = display_str(&out);
        assert!(
            s.contains("... FAILED"),
            "`test <name> ... FAILED` line must be preserved"
        );
    }

    #[test]
    fn test_mixed_multi_binary_failure_block_preserved() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(MIXED_MULTI_BINARY, &ctx);
        let s = display_str(&out);
        assert!(
            s.contains("---- ") && s.contains(" stdout ----"),
            "failure block header must be preserved"
        );
        // The block content (panic message) must also survive.
        assert!(
            s.contains("panicked at"),
            "failure block panic message must be preserved"
        );
    }

    #[test]
    fn test_mixed_multi_binary_failure_names_list_preserved() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(MIXED_MULTI_BINARY, &ctx);
        let s = display_str(&out);
        assert!(
            s.contains("failures:"),
            "`failures:` section must be preserved"
        );
    }

    #[test]
    fn test_mixed_multi_binary_ok_progress_lines_removed() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(MIXED_MULTI_BINARY, &ctx);
        let s = display_str(&out);
        assert!(
            !s.contains("... ok"),
            "`test <name> ... ok` progress lines must be removed"
        );
    }

    #[test]
    fn test_mixed_multi_binary_no_fabricated_ok_summary() {
        // The candidate must never contain a fabricated passing summary that
        // is not in the raw (e.g. a count mismatch or synthetic `ok.` line).
        // We verify that exactly ONE `test result: ok` appears (the first binary)
        // and exactly ONE `test result: FAILED` appears (the second binary).
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(MIXED_MULTI_BINARY, &ctx);
        let s = display_str(&out);
        let ok_count = s.matches("test result: ok.").count();
        let fail_count = s.matches("test result: FAILED.").count();
        assert_eq!(
            ok_count, 1,
            "exactly one passing summary expected, got {ok_count}"
        );
        assert_eq!(
            fail_count, 1,
            "exactly one failing summary expected, got {fail_count}"
        );
    }

    #[test]
    fn test_mixed_multi_binary_never_worse() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(MIXED_MULTI_BINARY, &ctx);
        assert!(
            out.display.len() < MIXED_MULTI_BINARY.len(),
            "candidate ({} bytes) must be strictly smaller than raw ({} bytes)",
            out.display.len(),
            MIXED_MULTI_BINARY.len()
        );
    }

    #[test]
    fn test_mixed_multi_binary_no_marker_in_candidate() {
        let cmd = make_cmd(vec!["test"]);
        let ctx = stdout_ctx(&cmd);
        let out = CargoTestReducer.reduce(MIXED_MULTI_BINARY, &ctx);
        let s = display_str(&out);
        assert!(
            !s.contains("[foldback"),
            "candidate must not contain foldback marker (pipeline appends it)"
        );
    }
}
