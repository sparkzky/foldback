/// Pytest-specific reducer.
///
/// Extracts failure/error blocks, the short-test-summary section, a warnings
/// summary (capped at 5 content lines), and the final summary line from
/// `pytest` stdout.  All other output (progress dots, PASSED lines, header
/// preamble) is discarded.
///
/// **Pure function** — no IO, no spawn, no exit-code mutation.
/// The pipeline (display/mod.rs) appends the rawref marker after checking
/// never-worse; the candidate body returned here must NOT contain a marker.
use crate::argv::NormalizedCommand;
use crate::display::context::ChannelContext;
use crate::display::outcome::{Recoverability, ReduceOutcome, ReductionKind, SkipReason, ViewKind};
use crate::display::registry::Reducer;
use crate::stash::Channel;

/// Maximum warning content lines to include in the summary view.
const MAX_WARNING_LINES: usize = 5;

pub struct PytestReducer;

impl Reducer for PytestReducer {
    fn name(&self) -> &'static str {
        "pytest"
    }

    fn matches(&self, norm: &NormalizedCommand) -> bool {
        matches!(norm, NormalizedCommand::Pytest { .. })
    }

    fn reduce(&self, input: &[u8], ctx: &ChannelContext) -> ReduceOutcome {
        // Only specialise stdout; let stderr fall through to generic/raw.
        if ctx.channel != Channel::Stdout {
            return ReduceOutcome::skipped(SkipReason::ParseFailed);
        }

        // Empty input.
        if input.is_empty() {
            return ReduceOutcome::skipped(SkipReason::Empty);
        }

        // Require valid UTF-8.
        let text = match std::str::from_utf8(input) {
            Ok(s) => s,
            Err(_) => return ReduceOutcome::skipped(SkipReason::NonUtf8),
        };

        // Passthrough gate — machine-readable flags bypass specialist parsing.
        if let Some(reason) = check_gate(ctx) {
            return ReduceOutcome::skipped(reason);
        }

        // Parse and reduce.
        match parse_and_reduce(text) {
            Some(candidate) => ReduceOutcome {
                display: candidate.into_bytes(),
                applied: true,
                view: ViewKind::PytestSummary,
                reduction: ReductionKind::SemanticSummary,
                recoverability: Recoverability::Retrievable,
                skip_reason: None,
            },
            None => ReduceOutcome::skipped(SkipReason::ParseFailed),
        }
    }
}

// ─── Passthrough gate ─────────────────────────────────────────────────────────

/// Return `Some(MachineReadable)` when args signal machine-oriented output.
fn check_gate(ctx: &ChannelContext) -> Option<SkipReason> {
    for arg in &ctx.command.args {
        let s = arg.as_str();
        if matches!(s, "--collect-only" | "--co" | "--verbose") {
            return Some(SkipReason::MachineReadable);
        }
        // Single-dash verbosity: -v, -vv, -vvv, … (one dash + one-or-more 'v's only).
        // Must not match -version, -value, etc.
        if is_verbosity_flag(s) {
            return Some(SkipReason::MachineReadable);
        }
        // Prefix matches for flags that may include a value (e.g. --junitxml=report.xml)
        if s.starts_with("--json-report")
            || s.starts_with("--junitxml")
            || s.starts_with("--junit-xml")
        {
            return Some(SkipReason::MachineReadable);
        }
    }
    None
}

/// Return `true` iff `s` is a single-dash verbosity flag: exactly `-` followed
/// by one or more `v` characters and nothing else (e.g. `-v`, `-vv`, `-vvv`).
///
/// Explicitly rejects `--verbose` (double-dash, handled separately), `-version`,
/// and any flag containing non-`v` characters after the dash.
fn is_verbosity_flag(s: &str) -> bool {
    match s.strip_prefix('-') {
        Some(rest) if !rest.is_empty() && !rest.starts_with('-') => rest.chars().all(|c| c == 'v'),
        _ => false,
    }
}

// ─── Parser ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Before any recognised section (header, progress dots, PASSED lines).
    Preamble,
    InFailures,
    InErrors,
    InShortSummary,
    /// Inside the `warnings summary` section; we cap content lines.
    InWarnings,
}

/// Try to extract the useful sections from pytest stdout.
///
/// Returns `Some(candidate)` on success, `None` to signal ParseFailed.
fn parse_and_reduce(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();

    let mut state = State::Preamble;

    // Accumulated section lines (including the section-header line itself).
    let mut failures: Vec<&str> = Vec::new();
    let mut errors: Vec<&str> = Vec::new();
    let mut short_summary: Vec<&str> = Vec::new();
    let mut warnings_header: Option<&str> = None;
    let mut warnings_content: Vec<&str> = Vec::new();
    let mut warnings_total: usize = 0;

    // Critical error lines that appear in the preamble (collection errors, etc.)
    let mut preamble_errors: Vec<&str> = Vec::new();

    let mut final_summary: Option<&str> = None;

    for line in &lines {
        // ── Final summary line (must check before generic section header) ───
        if is_final_summary(line) {
            final_summary = Some(line);
            state = State::Preamble; // nothing more to capture
            continue;
        }

        // ── Major section headers (= ... =) ─────────────────────────────────
        if is_major_section_header(line) {
            let title = section_inner(line);
            if title_contains_ci(title, "FAILURES") {
                state = State::InFailures;
                failures.push(line);
            } else if title_contains_ci(title, "ERRORS") {
                state = State::InErrors;
                errors.push(line);
            } else if title_contains_ci(title, "short test summary") {
                state = State::InShortSummary;
                short_summary.push(line);
            } else if title_contains_ci(title, "warnings summary") {
                state = State::InWarnings;
                warnings_header = Some(line);
            } else {
                // Unknown section: stop accumulating current section.
                state = State::Preamble;
            }
            continue;
        }

        // ── Accumulate based on current state ────────────────────────────────
        match state {
            State::Preamble => {
                // Keep ERROR lines from the progress area (collection errors).
                if line.starts_with("ERROR ") {
                    preamble_errors.push(line);
                }
            }
            State::InFailures => failures.push(line),
            State::InErrors => errors.push(line),
            State::InShortSummary => short_summary.push(line),
            State::InWarnings => {
                // Skip the pytest "-- Docs: ..." footer that appears at the end of
                // every warnings section.  It is metadata, not a warning entry, and
                // must not be counted toward the omitted-warnings total.
                if line.starts_with("-- Docs:") {
                    // intentionally excluded from count and content
                } else {
                    warnings_total += 1;
                    if warnings_content.len() < MAX_WARNING_LINES {
                        warnings_content.push(line);
                    }
                }
            }
        }
    }

    // No credible final summary → cannot produce a reliable specialist view.
    let summary = final_summary?;

    // Build output parts.
    let mut parts: Vec<String> = Vec::new();

    // Preamble ERROR lines (only when no ERRORS section captured them already).
    if errors.is_empty() && !preamble_errors.is_empty() {
        parts.push(preamble_errors.join("\n"));
    }

    if !failures.is_empty() {
        parts.push(failures.join("\n"));
    }

    if !errors.is_empty() {
        parts.push(errors.join("\n"));
    }

    if !short_summary.is_empty() {
        parts.push(short_summary.join("\n"));
    }

    if let Some(header) = warnings_header {
        let mut w = String::new();
        w.push_str(header);
        for wl in &warnings_content {
            w.push('\n');
            w.push_str(wl);
        }
        let omitted = warnings_total.saturating_sub(warnings_content.len());
        if omitted > 0 {
            w.push('\n');
            w.push_str(&format!("  ... ({omitted} warnings omitted)"));
        }
        parts.push(w);
    }

    parts.push(summary.to_string());

    // Ensure the candidate body ends with '\n' so the pipeline-appended marker
    // is placed on its own line (never concatenated with the final summary).
    Some(parts.join("\n") + "\n")
}

// ─── Line classification helpers ─────────────────────────────────────────────

/// Return `true` when `line` is a pytest final-summary line in either format:
/// - **Bordered**: `====== N passed in Xs ======` (pytest default)
/// - **Bare**: `N passed in Xs`  or  `1 failed, N passed in Xs`  (pytest -q)
fn is_final_summary(line: &str) -> bool {
    is_bordered_final_summary(line) || is_bare_final_summary(line)
}

/// Bordered format: `====== <summary> ======`
///
/// Summary inner text must start with a digit or "no tests ran".
fn is_bordered_final_summary(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.starts_with('=') || !trimmed.ends_with('=') {
        return false;
    }
    let inner = trimmed.trim_matches('=').trim();
    if inner.is_empty() {
        return false;
    }
    let first_digit = inner
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false);
    first_digit || inner.starts_with("no tests ran")
}

/// Bare format (pytest -q): a single trimmed line that encodes the run result
/// **without** `=` borders.
///
/// Accepted grammar (conservative; rejects tracebacks, assert messages, log lines):
/// ```text
/// bare-summary  = status-list " in " duration
///               | "no tests ran in " duration
/// status-list   = status-item ("," status-item)*
/// status-item   = NON-NEG-INT " " STATUS-WORD
/// STATUS-WORD   = "passed" | "failed" | "error" | "errors" | "skipped"
///               | "xfailed" | "xpassed" | "warning" | "warnings" | "deselected"
/// duration      = <chars: 0-9, '.', 'm', 'h', ' '> ending with 's'
/// ```
///
/// The first character of the trimmed line must be a digit (or "no tests ran …").
/// This immediately rejects lines from tracebacks (`E`, `>`, spaces), assert
/// statements starting with letters, and log-prefixed lines like `[LOG] 1 passed`.
fn is_bare_final_summary(line: &str) -> bool {
    let s = line.trim();
    if s.is_empty() {
        return false;
    }

    // "no tests ran in <duration>"
    if let Some(rest) = s.strip_prefix("no tests ran") {
        return rest.starts_with(" in ") && looks_like_duration(rest[" in ".len()..].trim());
    }

    // Must start with a digit — rejects tracebacks, assert lines, log prefixes.
    if !s.starts_with(|c: char| c.is_ascii_digit()) {
        return false;
    }

    // Split off " in <duration>" using the LAST occurrence of " in ".
    // (Using rfind guards against status words that happen to contain "in".)
    let in_sep = " in ";
    let in_pos = match s.rfind(in_sep) {
        Some(p) => p,
        None => return false,
    };

    let body = s[..in_pos].trim();
    let duration = s[in_pos + in_sep.len()..].trim();

    if !looks_like_duration(duration) {
        return false;
    }

    // Body must be a comma-separated list of "<non-neg-int> <STATUS-WORD>" pairs.
    for part in body.split(',') {
        let p = part.trim();
        let sp = match p.find(' ') {
            Some(i) => i,
            None => return false,
        };
        let count_str = &p[..sp];
        let status_str = p[sp + 1..].trim();
        if count_str.parse::<u64>().is_err() {
            return false;
        }
        if !is_pytest_status_word(status_str) {
            return false;
        }
    }

    true
}

/// `true` iff `s` looks like a pytest duration: at least two characters, ends
/// with `'s'`, and everything before the trailing `'s'` contains only ASCII
/// digits and the characters `'.'`, `'m'`, `'h'`, `' '`.
///
/// Accepts: `"0.12s"`, `"12.34s"`, `"1m 23.45s"`, `"1m23.45s"`, `"0s"`.
/// Rejects: `"s"` (too short), `"pytest.ini"` (invalid chars), `"12.34"` (no trailing 's').
fn looks_like_duration(s: &str) -> bool {
    // At least "Ns" (two chars): one digit + trailing 's'.
    if s.len() < 2 || !s.ends_with('s') {
        return false;
    }
    let prefix = &s[..s.len() - 1]; // everything before the terminal 's'
                                    // The prefix must contain at least one ASCII digit (rejects "ms", "hs", " s", …)
                                    // and every character must be a digit or a time-unit separator.
    prefix.chars().any(|c| c.is_ascii_digit())
        && prefix
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'm' | 'h' | ' '))
}

/// `true` iff `s` is a recognised pytest run-result status word.
fn is_pytest_status_word(s: &str) -> bool {
    matches!(
        s,
        "passed"
            | "failed"
            | "error"
            | "errors"
            | "skipped"
            | "xfailed"
            | "xpassed"
            | "warning"
            | "warnings"
            | "deselected"
    )
}

/// Return `true` for any `=` … `=` bordered line (major section header or summary).
fn is_major_section_header(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 5 && trimmed.starts_with("===") && trimmed.ends_with("===")
}

/// Extract the interior text of a `= … =` bordered line.
fn section_inner(line: &str) -> &str {
    line.trim().trim_matches('=').trim()
}

/// Case-insensitive contains check.
fn title_contains_ci(title: &str, needle: &str) -> bool {
    title
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::argv::NormalizedCommand;
    use crate::display::context::{ChannelContext, CommandContext};
    use crate::stash::Channel;

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
            ref_id: "abc123def456abc123def456abc123de",
            expires_at: &chrono::DateTime::<chrono::Utc>::MIN_UTC,
        }
    }

    // ── Reducer trait ──────────────────────────────────────────────────────────

    #[test]
    fn test_pytest_reducer_matches_pytest_normalized() {
        let r = PytestReducer;
        assert!(r.matches(&NormalizedCommand::Pytest {
            module_invocation: false
        }));
        assert!(r.matches(&NormalizedCommand::Pytest {
            module_invocation: true
        }));
    }

    #[test]
    fn test_pytest_reducer_does_not_match_other_commands() {
        let r = PytestReducer;
        assert!(!r.matches(&NormalizedCommand::CargoTest));
        assert!(!r.matches(&NormalizedCommand::Generic));
    }

    // ── is_final_summary ──────────────────────────────────────────────────────

    #[test]
    fn test_is_final_summary_passed() {
        assert!(is_final_summary(
            "============================== 120 passed in 5.43s =============================="
        ));
    }

    #[test]
    fn test_is_final_summary_failed() {
        assert!(is_final_summary(
            "====================== 1 failed, 100 passed in 3.12s ======================"
        ));
    }

    #[test]
    fn test_is_final_summary_no_tests_ran() {
        assert!(is_final_summary(
            "=============================== no tests ran ================================"
        ));
    }

    #[test]
    fn test_is_not_final_summary_failures_header() {
        assert!(!is_final_summary(
            "================================ FAILURES ================================"
        ));
    }

    #[test]
    fn test_is_not_final_summary_short_summary_header() {
        assert!(!is_final_summary(
            "=========================== short test summary info ============================"
        ));
    }

    // ── check_gate ────────────────────────────────────────────────────────────

    #[test]
    fn test_gate_collect_only() {
        let cmd = make_cmd(&["--collect-only"]);
        let ctx = make_stdout_ctx(&cmd);
        assert_eq!(check_gate(&ctx), Some(SkipReason::MachineReadable));
    }

    #[test]
    fn test_gate_co_short() {
        let cmd = make_cmd(&["--co"]);
        let ctx = make_stdout_ctx(&cmd);
        assert_eq!(check_gate(&ctx), Some(SkipReason::MachineReadable));
    }

    #[test]
    fn test_gate_verbose() {
        let cmd = make_cmd(&["-v"]);
        let ctx = make_stdout_ctx(&cmd);
        assert_eq!(check_gate(&ctx), Some(SkipReason::MachineReadable));
    }

    #[test]
    fn test_gate_no_flags_passes() {
        let cmd = make_cmd(&["-q", "tests/"]);
        let ctx = make_stdout_ctx(&cmd);
        assert_eq!(check_gate(&ctx), None);
    }

    // ── is_verbosity_flag ─────────────────────────────────────────────────────

    #[test]
    fn test_is_verbosity_flag_v() {
        assert!(is_verbosity_flag("-v"));
    }

    #[test]
    fn test_is_verbosity_flag_vv() {
        assert!(is_verbosity_flag("-vv"));
    }

    #[test]
    fn test_is_verbosity_flag_vvv() {
        assert!(is_verbosity_flag("-vvv"));
    }

    #[test]
    fn test_is_verbosity_flag_version_rejected() {
        assert!(!is_verbosity_flag("-version"));
    }

    #[test]
    fn test_is_verbosity_flag_double_dash_rejected() {
        assert!(!is_verbosity_flag("--verbose"));
        assert!(!is_verbosity_flag("--vv"));
    }

    #[test]
    fn test_is_verbosity_flag_empty_rejected() {
        assert!(!is_verbosity_flag(""));
        assert!(!is_verbosity_flag("-"));
    }

    #[test]
    fn test_gate_vv_machine_readable() {
        let cmd = make_cmd(&["-vv"]);
        let ctx = make_stdout_ctx(&cmd);
        assert_eq!(check_gate(&ctx), Some(SkipReason::MachineReadable));
    }

    #[test]
    fn test_gate_vvv_machine_readable() {
        let cmd = make_cmd(&["-vvv"]);
        let ctx = make_stdout_ctx(&cmd);
        assert_eq!(check_gate(&ctx), Some(SkipReason::MachineReadable));
    }

    // ── parse_and_reduce: candidate trailing newline ──────────────────────────

    #[test]
    fn test_parse_and_reduce_ends_with_newline() {
        let text = "\
            ============================= test session starts ==============================\n\
            platform linux\n\
            collected 1 item\n\
            \n\
            tests/test_m.py::test_a PASSED                                            [100%]\n\
            \n\
            ============================== 1 passed in 0.01s ==============================\n";
        let result = parse_and_reduce(text);
        assert!(result.is_some(), "must parse successfully");
        assert!(
            result.unwrap().ends_with('\n'),
            "parse_and_reduce output must end with newline"
        );
    }

    // ── parse_and_reduce: Docs footer excluded from warnings ──────────────────

    #[test]
    fn test_docs_footer_not_counted_in_warnings() {
        // 5 warnings + Docs footer; with the fix, omitted must be 0.
        let mut text = String::new();
        text.push_str(
            "============================= test session starts ==============================\n",
        );
        text.push_str("platform linux\n");
        text.push_str("collected 10 items\n\n");
        for i in 1..=10 {
            text.push_str(&format!("tests/t.py::test_{i} PASSED [ {i}%]\n"));
        }
        text.push('\n');
        text.push_str(
            "============================= warnings summary ==============================\n",
        );
        for i in 1..=5 {
            text.push_str(&format!("  /lib.py:{i}: DeprecationWarning: warn{i}\n"));
        }
        text.push_str("-- Docs: https://docs.pytest.org/en/stable/how-to/capture-warnings.html\n");
        text.push_str(
            "============================== 10 passed in 0.01s ==============================\n",
        );

        let result = parse_and_reduce(&text).expect("must parse");
        // Must NOT contain omission line (Docs footer must not push count to 6).
        assert!(
            !result.contains("warnings omitted"),
            "Docs footer must not be counted; no omission line expected"
        );
        // Must contain the 5 real DeprecationWarning lines.
        assert_eq!(result.matches("DeprecationWarning").count(), 5);
        // Docs footer must not appear.
        assert!(!result.contains("-- Docs:"));
    }

    // ── is_bare_final_summary: positive grammar ───────────────────────────────

    #[test]
    fn test_bare_summary_accept_simple_passed() {
        assert!(is_bare_final_summary("5000 passed in 12.34s"));
        assert!(is_bare_final_summary("1 passed in 0.12s"));
        assert!(is_bare_final_summary("0 passed in 0s"));
    }

    #[test]
    fn test_bare_summary_accept_mixed_statuses() {
        assert!(is_bare_final_summary("1 failed, 4950 passed in 15.43s"));
        assert!(is_bare_final_summary(
            "2 errors, 1 failed, 97 passed in 1.23s"
        ));
        assert!(is_bare_final_summary("5 skipped in 0.1s"));
        assert!(is_bare_final_summary(
            "3 xfailed, 2 xpassed, 10 passed in 0.5s"
        ));
        assert!(is_bare_final_summary("1 deselected, 99 passed in 0.9s"));
        assert!(is_bare_final_summary("2 warnings, 50 passed in 0.3s"));
    }

    #[test]
    fn test_bare_summary_accept_no_tests_ran() {
        assert!(is_bare_final_summary("no tests ran in 0.01s"));
        assert!(is_bare_final_summary("no tests ran in 1m 23.45s"));
    }

    #[test]
    fn test_bare_summary_accept_minutes_duration() {
        assert!(is_bare_final_summary("1000 passed in 1m 30.00s"));
        assert!(is_bare_final_summary("500 passed in 2m3.45s"));
    }

    // ── is_bare_final_summary: negative grammar ───────────────────────────────

    #[test]
    fn test_bare_summary_reject_no_duration() {
        assert!(!is_bare_final_summary("1 passed"));
        assert!(!is_bare_final_summary("100 passed, 5 failed"));
        assert!(!is_bare_final_summary("no tests ran"));
    }

    #[test]
    fn test_bare_summary_reject_unknown_status_word() {
        // "requests handled" is not a pytest status
        assert!(!is_bare_final_summary("5 requests handled in 0.1s"));
        assert!(!is_bare_final_summary("1 custom in 0.1s"));
    }

    #[test]
    fn test_bare_summary_reject_assert_line() {
        // Lines starting with letters, not digits
        assert!(!is_bare_final_summary("assert \"1 passed in 0.1s\""));
        assert!(!is_bare_final_summary(
            "assert result == \"1 passed in 0.1s\""
        ));
        assert!(!is_bare_final_summary(
            "E   AssertionError: 1 passed in 0.1s"
        ));
    }

    #[test]
    fn test_bare_summary_reject_extra_prefix() {
        // Prefixes before the digit
        assert!(!is_bare_final_summary("[LOG] 1 passed in 0.12s"));
        assert!(!is_bare_final_summary("2026-01-01 1 passed in 0.12s"));
    }

    #[test]
    fn test_bare_summary_reject_malformed_count() {
        assert!(!is_bare_final_summary("abc passed in 0.1s"));
        assert!(!is_bare_final_summary(" passed in 0.1s"));
    }

    #[test]
    fn test_bare_summary_reject_non_duration_suffix() {
        // Duration that doesn't end with 's'
        assert!(!is_bare_final_summary("1 passed in 12.34"));
        // Duration containing non-duration characters
        assert!(!is_bare_final_summary("1 passed in pytest.ini"));
    }

    // ── looks_like_duration: digit requirement (LOW fix) ─────────────────────
    //
    // "ms" / "hs" / " s" end with 's' and their prefix chars are all in the
    // allowlist, but the prefix contains no digit — must be rejected.
    // These tests are RED until the "at least one digit" guard is added.

    #[test]
    fn test_duration_reject_pure_unit_ms() {
        // "ms" prefix="m" — no digit → must be false
        assert!(
            !looks_like_duration("ms"),
            "looks_like_duration('ms') must be false (no digit in prefix)"
        );
    }

    #[test]
    fn test_duration_reject_pure_unit_hs() {
        assert!(
            !looks_like_duration("hs"),
            "looks_like_duration('hs') must be false"
        );
    }

    #[test]
    fn test_duration_reject_space_s() {
        assert!(
            !looks_like_duration(" s"),
            "looks_like_duration(' s') must be false (space-only prefix)"
        );
    }

    #[test]
    fn test_bare_summary_reject_duration_ms() {
        // "10 passed in ms" — "ms" is not a valid duration → ParseFailed
        assert!(
            !is_bare_final_summary("10 passed in ms"),
            "'10 passed in ms' must not be a recognised final summary"
        );
    }

    // Positive: real durations must still pass.
    #[test]
    fn test_duration_accept_canonical_forms() {
        assert!(looks_like_duration("0.12s"), "0.12s");
        assert!(looks_like_duration("12.34s"), "12.34s");
        assert!(looks_like_duration("0s"), "0s");
        assert!(looks_like_duration("1m 23.45s"), "1m 23.45s");
        assert!(looks_like_duration("2m3.45s"), "2m3.45s");
        assert!(looks_like_duration("1h 2m 3.0s"), "1h 2m 3.0s");
    }

    #[test]
    fn test_bare_summary_reject_empty() {
        assert!(!is_bare_final_summary(""));
        assert!(!is_bare_final_summary("   "));
    }

    // ── is_bordered_final_summary: unchanged behaviour ────────────────────────

    #[test]
    fn test_bordered_final_summary_still_matches() {
        assert!(is_bordered_final_summary(
            "============================== 10 passed in 0.1s =============================="
        ));
        assert!(is_bordered_final_summary(
            "======= no tests ran in 0.01s ======="
        ));
    }

    // ── Stub compatibility (existing tests from stub) ────────────────────────

    #[test]
    fn test_pytest_reducer_does_not_apply_to_stderr() {
        let cmd = make_cmd(&[]);
        let ctx = ChannelContext {
            command: &cmd,
            channel: Channel::Stderr,
            ref_id: "abc123def456abc123def456abc123de",
            expires_at: &chrono::DateTime::<chrono::Utc>::MIN_UTC,
        };
        let outcome = PytestReducer.reduce(b"some pytest output", &ctx);
        assert!(outcome.skip_reason.is_some(), "stderr must be skipped");
        assert!(!outcome.applied);
    }
}
