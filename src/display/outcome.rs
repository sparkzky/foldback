/// Which view of the output was produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewKind {
    /// Phase 1 generic head/tail condenser output (marker contains `omitted=`).
    Generic,
    /// Semantic pytest summary (marker contains `view=pytest`).
    PytestSummary,
    /// Semantic cargo test summary (marker contains `view=cargo-test`).
    CargoTestSummary,
    /// Raw bytes — no condensing applied.
    Raw,
}

impl ViewKind {
    /// Marker `view=` value for this kind.  Only meaningful for specialized views.
    pub fn marker_name(&self) -> &'static str {
        match self {
            ViewKind::PytestSummary => "pytest",
            ViewKind::CargoTestSummary => "cargo-test",
            ViewKind::Generic | ViewKind::Raw => "",
        }
    }
}

/// How the inline (terminal-visible) bytes were produced.
///
/// Orthogonal to `Recoverability` — inline lossiness does not prevent raw recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReductionKind {
    /// Generic head/tail truncation; `omitted=` count is exact and verifiable.
    GenericTruncation,
    /// Semantic summary (pytest/cargo); inline display is lossy — some lines dropped.
    SemanticSummary,
}

/// End-to-end recoverability of the raw output.
///
/// `Retrievable` means the full raw bytes are stored in Stash and reachable
/// via `foldback output get`.  It does **not** imply the inline display is lossless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recoverability {
    Retrievable,
}

/// Why a reducer (or the whole pipeline) chose not to apply specialised reduction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// `FOLDBACK_REDUCERS=0` — specialised reducers disabled at runtime.
    Disabled,
    /// No registered reducer matches this `NormalizedCommand`.
    NoMatch,
    /// Reducer matched but could not parse the output.
    ParseFailed,
    /// Output contains machine-readable flags (e.g. `--collect-only`, `--json`).
    MachineReadable,
    /// Condensed form is not strictly smaller than raw (never-worse check failed).
    NoBenefit,
    /// Output is not valid UTF-8; reducer requires text.
    NonUtf8,
    /// Output is empty; nothing to condense.
    Empty,
}

/// Result of the display pipeline for a single channel.
///
/// Returned by both `Reducer::reduce` (without marker) and `render_channel`
/// (with marker appended when `applied = true`).
#[derive(Debug, Clone)]
pub struct ReduceOutcome {
    /// Bytes to write to the terminal.
    ///
    /// When `applied = true` and produced by `render_channel`, this already
    /// contains the marker bytes appended at the end.
    pub display: Vec<u8>,
    /// `true` if condensing was applied and display differs from raw.
    pub applied: bool,
    /// Which view was produced.
    pub view: ViewKind,
    /// How the inline bytes were reduced.
    pub reduction: ReductionKind,
    /// Whether the full raw bytes are recoverable via `output get`.
    pub recoverability: Recoverability,
    /// Set when the reducer (or pipeline) chose to skip.
    pub skip_reason: Option<SkipReason>,
}

impl ReduceOutcome {
    /// Construct a skipped outcome (reducer did not apply).
    pub fn skipped(reason: SkipReason) -> Self {
        Self {
            display: Vec::new(),
            applied: false,
            view: ViewKind::Raw,
            reduction: ReductionKind::GenericTruncation,
            recoverability: Recoverability::Retrievable,
            skip_reason: Some(reason),
        }
    }

    /// Construct a raw passthrough outcome (no condensing applied).
    pub fn raw_passthrough(data: Vec<u8>) -> Self {
        Self {
            display: data,
            applied: false,
            view: ViewKind::Raw,
            reduction: ReductionKind::GenericTruncation,
            recoverability: Recoverability::Retrievable,
            skip_reason: None,
        }
    }
}
