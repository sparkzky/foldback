pub mod context;
pub mod generic;
pub mod marker;
pub mod outcome;
pub mod reducers;
pub mod registry;

use chrono::{DateTime, Utc};

use crate::stash::Channel;
use context::{ChannelContext, CommandContext};
use outcome::{Recoverability, ReduceOutcome, ReductionKind, ViewKind};
use registry::Registry;

/// Return `true` when the condensed view is strictly smaller than raw.
///
/// Marker bytes are part of `display` and are counted here (never-worse per §2.3).
pub fn beneficial(display: &[u8], raw: &[u8]) -> bool {
    display.len() < raw.len()
}

/// Produce the display output for a single I/O channel.
///
/// Pipeline (§5.2):
/// 1. Below threshold → raw passthrough.
/// 2. If `reducers_enabled`: try specialized reducers in registry order.
///    - Skip reason → continue to next reducer.
///    - Applied but not beneficial (including marker bytes) → fall through to generic.
///    - Applied and beneficial → return (display has marker appended).
/// 3. Generic (`condenser::condense`) — same algorithm as Phase 1, byte-identical output.
/// 4. If generic not beneficial → raw passthrough.
pub fn render_channel(
    raw: &[u8],
    ctx: &ChannelContext,
    registry: &Registry,
    reducers_enabled: bool,
) -> ReduceOutcome {
    // Step 1: below threshold → raw passthrough (identical to condenser logic)
    if !generic::exceeds_threshold(raw) {
        return ReduceOutcome::raw_passthrough(raw.to_vec());
    }

    // Step 2: try specialized reducers (only when enabled)
    if reducers_enabled {
        if let Some(outcome) = try_specialized(raw, ctx, registry) {
            return outcome;
        }
    }

    // Step 3: generic fallback — delegate entirely to Phase 1 condenser for byte-exact compat
    let cond = crate::condenser::condense(raw, ctx.ref_id, ctx.expires_at);
    if cond.condensed {
        return ReduceOutcome {
            display: cond.display,
            applied: true,
            view: ViewKind::Generic,
            reduction: ReductionKind::GenericTruncation,
            recoverability: Recoverability::Retrievable,
            skip_reason: None,
        };
    }

    // Step 4: generic also not beneficial → raw passthrough
    ReduceOutcome::raw_passthrough(raw.to_vec())
}

/// Try each matching specialized reducer in registry order.
///
/// Returns `Some(outcome)` if a reducer applied and was beneficial (never-worse check
/// including the appended marker bytes).  Returns `None` to fall through to generic.
fn try_specialized(raw: &[u8], ctx: &ChannelContext, registry: &Registry) -> Option<ReduceOutcome> {
    for reducer in registry.match_reducers(&ctx.command.normalized) {
        let candidate = reducer.reduce(raw, ctx);

        // Any skip reason → try next reducer or fall through
        if candidate.skip_reason.is_some() {
            continue;
        }

        if candidate.applied {
            // Build specialized marker and append to candidate display
            let view_name = candidate.view.marker_name();
            let marker_bytes =
                marker::build_specialized_marker(ctx.ref_id, raw, view_name, ctx.expires_at);

            let mut full_display = candidate.display;
            full_display.extend_from_slice(&marker_bytes);

            // Never-worse check: specialized + marker must be strictly smaller than raw
            if beneficial(&full_display, raw) {
                return Some(ReduceOutcome {
                    display: full_display,
                    applied: true,
                    view: candidate.view,
                    reduction: candidate.reduction,
                    recoverability: candidate.recoverability,
                    skip_reason: None,
                });
            }
            // Not beneficial after marker → fall through to generic (don't try further reducers)
            return None;
        }

        // Reducer returned applied=false with no skip_reason — treat as skip
        continue;
    }
    None
}

/// Produce display output for both stdout and stderr channels.
///
/// Constructs `ChannelContext` for each channel and calls `render_channel`.
pub fn render_passthrough(
    stdout: &[u8],
    stderr: &[u8],
    cmd_ctx: &CommandContext,
    ref_id: &str,
    expires_at: &DateTime<Utc>,
    registry: &Registry,
    reducers_enabled: bool,
) -> (ReduceOutcome, ReduceOutcome) {
    let stdout_ctx = ChannelContext {
        command: cmd_ctx,
        channel: Channel::Stdout,
        ref_id,
        expires_at,
    };
    let stderr_ctx = ChannelContext {
        command: cmd_ctx,
        channel: Channel::Stderr,
        ref_id,
        expires_at,
    };

    let stdout_out = render_channel(stdout, &stdout_ctx, registry, reducers_enabled);
    let stderr_out = render_channel(stderr, &stderr_ctx, registry, reducers_enabled);

    (stdout_out, stderr_out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::argv::NormalizedCommand;
    use crate::condenser;
    use crate::display::context::{ChannelContext, CommandContext};
    use crate::display::outcome::{
        Recoverability, ReduceOutcome, ReductionKind, SkipReason, ViewKind,
    };
    use crate::display::registry::{Reducer, Registry};
    use crate::stash::Channel;
    use chrono::TimeZone;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    const REF_ID: &str = "abc123def456abc123def456abc123de";

    fn expires() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc.with_ymd_and_hms(2026, 9, 7, 0, 0, 0).unwrap()
    }

    fn numbered_lines(count: usize) -> Vec<u8> {
        (0..count)
            .flat_map(|i| format!("line-{i:04}\n").into_bytes())
            .collect()
    }

    fn make_generic_ctx_with_expires<'a>(
        cmd: &'a CommandContext,
        expires: &'a chrono::DateTime<chrono::Utc>,
    ) -> ChannelContext<'a> {
        ChannelContext {
            command: cmd,
            channel: Channel::Stdout,
            ref_id: REF_ID,
            expires_at: expires,
        }
    }

    fn generic_cmd() -> CommandContext {
        CommandContext {
            command: "seq".to_string(),
            args: vec![],
            normalized: NormalizedCommand::Generic,
            exit_code: 0,
            cwd: ".".to_string(),
        }
    }

    // ── never-worse (beneficial) ──────────────────────────────────────────────

    #[test]
    fn test_beneficial_strictly_smaller() {
        assert!(beneficial(b"ab", b"abc"));
    }

    #[test]
    fn test_beneficial_equal_is_not_beneficial() {
        assert!(!beneficial(b"abc", b"abc"));
    }

    #[test]
    fn test_beneficial_larger_is_not_beneficial() {
        assert!(!beneficial(b"abcd", b"abc"));
    }

    // ── generic output byte-identical to old condenser ────────────────────────

    #[test]
    fn test_render_channel_generic_short_passthrough() {
        let data = b"short output\n";
        let cmd = generic_cmd();
        let exp = expires();
        let ctx = make_generic_ctx_with_expires(&cmd, &exp);
        let registry = Registry::new(vec![]);
        let out = render_channel(data, &ctx, &registry, false);

        assert!(!out.applied, "short output must not be condensed");
        assert_eq!(out.display, data, "short output must passthrough unchanged");
        assert_eq!(out.view, ViewKind::Raw);
    }

    #[test]
    fn test_render_channel_generic_long_equals_condenser() {
        let data = numbered_lines(200);
        let cmd = generic_cmd();
        let exp = expires();
        let ctx = make_generic_ctx_with_expires(&cmd, &exp);
        let registry = Registry::new(vec![]);

        let old = condenser::condense(&data, REF_ID, &exp);
        let new_out = render_channel(&data, &ctx, &registry, false);

        assert!(old.condensed, "condenser must condense 200 lines");
        assert!(new_out.applied, "pipeline must condense 200 lines");
        assert_eq!(
            new_out.display, old.display,
            "pipeline generic output must be byte-identical to condenser output"
        );
        assert_eq!(new_out.view, ViewKind::Generic);
    }

    #[test]
    fn test_render_channel_generic_no_benefit_passthrough() {
        // Huge single line — head+marker+tail is NOT shorter
        let data = vec![b'x'; 1024 * 12]; // 12 KB single line
        let cmd = generic_cmd();
        let exp = expires();
        let ctx = make_generic_ctx_with_expires(&cmd, &exp);
        let registry = Registry::new(vec![]);

        let old = condenser::condense(&data, REF_ID, &exp);
        let new_out = render_channel(&data, &ctx, &registry, false);

        assert!(
            !old.condensed,
            "condenser must not condense huge single line"
        );
        assert!(
            !new_out.applied,
            "pipeline must not condense huge single line"
        );
        assert_eq!(new_out.display, data, "must passthrough unchanged");
    }

    // ── FOLDBACK_REDUCERS=0 / reducers_enabled=false ──────────────────────────

    /// When `reducers_enabled=false`, the registry must not be queried.
    #[test]
    fn test_render_channel_reducers_disabled_does_not_call_registry() {
        struct PanicReducer;
        impl Reducer for PanicReducer {
            fn name(&self) -> &'static str {
                "panic"
            }
            fn matches(&self, _: &NormalizedCommand) -> bool {
                true
            }
            fn reduce(&self, _: &[u8], _: &ChannelContext) -> ReduceOutcome {
                panic!("reducer must not be called when reducers_enabled=false")
            }
        }

        let data = numbered_lines(200);
        let cmd = CommandContext {
            command: "pytest".to_string(),
            args: vec![],
            normalized: NormalizedCommand::Pytest {
                module_invocation: false,
            },
            exit_code: 0,
            cwd: ".".to_string(),
        };
        let exp = expires();
        let ctx = make_generic_ctx_with_expires(&cmd, &exp);
        let registry = Registry::new(vec![Box::new(PanicReducer)]);

        // reducers_enabled=false → PanicReducer must NOT be called
        let out = render_channel(&data, &ctx, &registry, false);
        // Generic condenser is still applied (FOLDBACK_REDUCERS=0 only skips specialized)
        assert_eq!(
            out.view,
            ViewKind::Generic,
            "opt-out must still produce generic output"
        );
        let display_str = String::from_utf8_lossy(&out.display);
        assert!(
            !display_str.contains("view="),
            "FOLDBACK_REDUCERS=0 opt-out must not produce view= field"
        );
    }

    // ── registry IS called when reducers_enabled=true ─────────────────────────

    #[test]
    fn test_render_channel_reducers_enabled_calls_matching_reducer() {
        let called = Arc::new(AtomicBool::new(false));

        struct TrackReducer {
            called: Arc<AtomicBool>,
        }
        impl Reducer for TrackReducer {
            fn name(&self) -> &'static str {
                "track"
            }
            fn matches(&self, norm: &NormalizedCommand) -> bool {
                matches!(norm, NormalizedCommand::Pytest { .. })
            }
            fn reduce(&self, _input: &[u8], _ctx: &ChannelContext) -> ReduceOutcome {
                self.called.store(true, Ordering::SeqCst);
                ReduceOutcome::skipped(SkipReason::ParseFailed)
            }
        }

        let data = numbered_lines(200);
        let cmd = CommandContext {
            command: "pytest".to_string(),
            args: vec![],
            normalized: NormalizedCommand::Pytest {
                module_invocation: false,
            },
            exit_code: 0,
            cwd: ".".to_string(),
        };
        let exp = expires();
        let ctx = make_generic_ctx_with_expires(&cmd, &exp);
        let registry = Registry::new(vec![Box::new(TrackReducer {
            called: Arc::clone(&called),
        })]);

        let _out = render_channel(&data, &ctx, &registry, true);
        assert!(
            called.load(Ordering::SeqCst),
            "reducer must be called when reducers_enabled=true"
        );
    }

    // ── never-worse: candidate not smaller → fallback to generic ─────────────

    #[test]
    fn test_render_channel_candidate_larger_than_raw_falls_back_to_generic() {
        struct BigReducer;
        impl Reducer for BigReducer {
            fn name(&self) -> &'static str {
                "big"
            }
            fn matches(&self, _: &NormalizedCommand) -> bool {
                true
            }
            fn reduce(&self, input: &[u8], _ctx: &ChannelContext) -> ReduceOutcome {
                // Return a "candidate" larger than input (never-worse must reject it)
                let mut big = input.to_vec();
                for _ in 0..50 {
                    big.extend_from_slice(b"extra line to bloat output\n");
                }
                ReduceOutcome {
                    display: big,
                    applied: true,
                    view: ViewKind::PytestSummary,
                    reduction: ReductionKind::SemanticSummary,
                    recoverability: Recoverability::Retrievable,
                    skip_reason: None,
                }
            }
        }

        let data = numbered_lines(200);
        let cmd = CommandContext {
            command: "pytest".to_string(),
            args: vec![],
            normalized: NormalizedCommand::Pytest {
                module_invocation: false,
            },
            exit_code: 0,
            cwd: ".".to_string(),
        };
        let exp = expires();
        let ctx = make_generic_ctx_with_expires(&cmd, &exp);
        let registry = Registry::new(vec![Box::new(BigReducer)]);

        let out = render_channel(&data, &ctx, &registry, true);
        // BigReducer candidate + marker is not beneficial → fall back to generic
        assert_ne!(
            out.view,
            ViewKind::PytestSummary,
            "should NOT use BigReducer output"
        );
        let display_str = String::from_utf8_lossy(&out.display);
        assert!(
            !display_str.contains("view="),
            "generic output must not contain view="
        );
    }

    // ── render_passthrough ─────────────────────────────────────────────────────

    #[test]
    fn test_render_passthrough_produces_two_outcomes() {
        let stdout_data = numbered_lines(200);
        let stderr_data = b"short stderr\n";
        let cmd = generic_cmd();
        let exp = expires();
        let registry = Registry::new(vec![]);

        let (stdout_out, stderr_out) = render_passthrough(
            &stdout_data,
            stderr_data,
            &cmd,
            REF_ID,
            &exp,
            &registry,
            false,
        );

        // stdout condensed (200 lines > threshold)
        assert!(stdout_out.applied, "long stdout should be condensed");
        // stderr not condensed (short)
        assert!(!stderr_out.applied, "short stderr should not be condensed");
        assert_eq!(stderr_out.display, stderr_data);
    }
}
