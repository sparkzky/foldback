use crate::argv::NormalizedCommand;
use crate::display::context::ChannelContext;
use crate::display::outcome::ReduceOutcome;

/// Pure-function reducer interface.
///
/// Constraints (Phase 2 invariants):
/// - No `std::fs`, no `Command`, no network, no exit-code modification.
/// - `reduce` receives raw bytes and context; returns `ReduceOutcome`.
/// - When `skip_reason` is `Some`, `display` bytes are ignored by the pipeline.
pub trait Reducer: Send + Sync {
    /// Unique identifier for logging / debugging.
    fn name(&self) -> &'static str;
    /// Return `true` if this reducer handles the given normalized command.
    fn matches(&self, norm: &NormalizedCommand) -> bool;
    /// Attempt to reduce `input`.
    ///
    /// Returns a `ReduceOutcome` whose `display` contains the reduced content
    /// **without** the foldback marker (the pipeline appends the marker after
    /// checking never-worse).
    fn reduce(&self, input: &[u8], ctx: &ChannelContext) -> ReduceOutcome;
}

/// Ordered list of registered reducers.
///
/// Reducers are evaluated in insertion order; the first one that succeeds
/// (no `skip_reason`) and passes the never-worse check wins.
pub struct Registry {
    reducers: Vec<Box<dyn Reducer>>,
}

impl Registry {
    /// Construct a registry from an explicit list.
    pub fn new(reducers: Vec<Box<dyn Reducer>>) -> Self {
        Self { reducers }
    }

    /// Build the default production registry: pytest, then cargo-test.
    pub fn default_registry() -> Self {
        use crate::display::reducers::{cargo_test::CargoTestReducer, pytest::PytestReducer};
        Self::new(vec![Box::new(PytestReducer), Box::new(CargoTestReducer)])
    }

    /// Return references to all reducers whose `matches()` returns true.
    ///
    /// Results are in registration order (priority order).
    pub fn match_reducers(&self, norm: &NormalizedCommand) -> Vec<&dyn Reducer> {
        self.reducers
            .iter()
            .filter(|r| r.matches(norm))
            .map(|r| r.as_ref())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::argv::NormalizedCommand;
    use crate::display::context::{ChannelContext, CommandContext};
    use crate::display::outcome::{ReduceOutcome, SkipReason};
    use crate::stash::Channel;
    use chrono::Utc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn make_cmd_ctx(norm: NormalizedCommand) -> CommandContext {
        CommandContext {
            command: "test".to_string(),
            args: vec![],
            normalized: norm,
            exit_code: 0,
            cwd: ".".to_string(),
        }
    }

    fn make_channel_ctx<'a>(cmd: &'a CommandContext) -> ChannelContext<'a> {
        ChannelContext {
            command: cmd,
            channel: Channel::Stdout,
            ref_id: "abc123def456abc123def456abc123de",
            expires_at: &chrono::DateTime::<Utc>::MIN_UTC,
        }
    }

    struct TrackingReducer {
        called: Arc<AtomicBool>,
        norm_match: NormalizedCommand,
    }

    impl Reducer for TrackingReducer {
        fn name(&self) -> &'static str {
            "tracking"
        }

        fn matches(&self, norm: &NormalizedCommand) -> bool {
            norm == &self.norm_match
        }

        fn reduce(&self, _input: &[u8], _ctx: &ChannelContext) -> ReduceOutcome {
            self.called.store(true, Ordering::SeqCst);
            ReduceOutcome::skipped(SkipReason::ParseFailed)
        }
    }

    #[test]
    fn test_registry_match_reducers_calls_matching() {
        let called = Arc::new(AtomicBool::new(false));
        let registry = Registry::new(vec![Box::new(TrackingReducer {
            called: Arc::clone(&called),
            norm_match: NormalizedCommand::Pytest {
                module_invocation: false,
            },
        })]);

        let cmd = make_cmd_ctx(NormalizedCommand::Pytest {
            module_invocation: false,
        });
        let ctx = make_channel_ctx(&cmd);

        // Confirm reducer matches
        let matched = registry.match_reducers(&cmd.normalized);
        assert_eq!(
            matched.len(),
            1,
            "should match exactly one reducer for Pytest"
        );

        // Call reduce on the matched reducer
        matched[0].reduce(b"some data", &ctx);
        assert!(
            called.load(Ordering::SeqCst),
            "reducer must have been called"
        );
    }

    #[test]
    fn test_registry_match_reducers_skips_non_matching() {
        let called = Arc::new(AtomicBool::new(false));
        let registry = Registry::new(vec![Box::new(TrackingReducer {
            called: Arc::clone(&called),
            norm_match: NormalizedCommand::CargoTest,
        })]);

        let cmd = make_cmd_ctx(NormalizedCommand::Generic);
        let matched = registry.match_reducers(&cmd.normalized);
        assert_eq!(matched.len(), 0, "should not match any reducer for Generic");
    }

    #[test]
    fn test_registry_empty_returns_no_matches() {
        let registry = Registry::new(vec![]);
        let cmd = make_cmd_ctx(NormalizedCommand::Pytest {
            module_invocation: false,
        });
        assert_eq!(registry.match_reducers(&cmd.normalized).len(), 0);
    }

    #[test]
    fn test_default_registry_has_reducers() {
        let registry = Registry::default_registry();
        // Pytest matches
        let pytest_cmd = make_cmd_ctx(NormalizedCommand::Pytest {
            module_invocation: false,
        });
        assert!(!registry.match_reducers(&pytest_cmd.normalized).is_empty());
        // CargoTest matches
        let cargo_cmd = make_cmd_ctx(NormalizedCommand::CargoTest);
        assert!(!registry.match_reducers(&cargo_cmd.normalized).is_empty());
        // Generic has no match
        let generic_cmd = make_cmd_ctx(NormalizedCommand::Generic);
        assert!(registry.match_reducers(&generic_cmd.normalized).is_empty());
    }
}
