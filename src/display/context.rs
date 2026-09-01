use chrono::{DateTime, Utc};

use crate::argv::NormalizedCommand;
use crate::stash::Channel;

/// Per-invocation context built from `CaptureResult` after Stash save succeeds.
pub struct CommandContext {
    /// `argv[0]` as passed to rawref, unmodified.
    pub command: String,
    /// `argv[1..]` as passed to rawref.
    pub args: Vec<String>,
    /// Normalized form of `command` + `args`, used for reducer dispatch.
    pub normalized: NormalizedCommand,
    /// Exit code of the child process.
    pub exit_code: i32,
    /// Working directory at capture time.
    pub cwd: String,
}

/// Per-channel context threaded through `render_channel` and `Reducer::reduce`.
pub struct ChannelContext<'a> {
    /// Shared invocation context.
    pub command: &'a CommandContext,
    /// Which I/O channel this context is for.
    pub channel: Channel,
    /// Stash ref identifier (32 lowercase hex chars).
    pub ref_id: &'a str,
    /// When this ref expires; embedded in the marker.
    pub expires_at: &'a DateTime<Utc>,
}
