use crate::error::RawrefError;
use crate::stash::{Channel, Stash};

pub struct GetArgs {
    pub ref_id: String,
    pub channel: Channel,
    pub offset: Option<u64>,
    pub limit: Option<u64>,
}

/// Write the requested channel bytes to `out` without any condensing.
pub fn run(stash: &Stash, args: &GetArgs, out: &mut dyn std::io::Write) -> Result<(), RawrefError> {
    let data = stash.read_channel(&args.ref_id, args.channel, args.offset, args.limit)?;
    out.write_all(&data).map_err(RawrefError::Io)?;
    Ok(())
}
