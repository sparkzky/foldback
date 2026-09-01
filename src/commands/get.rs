use crate::error::FoldbackError;
use crate::stash::{Channel, Stash};

pub struct GetArgs {
    pub ref_id: String,
    pub channel: Channel,
    pub offset: Option<u64>,
    pub limit: Option<u64>,
}

/// Write the requested channel bytes to `out` without any condensing.
pub fn run(
    stash: &Stash,
    args: &GetArgs,
    out: &mut dyn std::io::Write,
) -> Result<(), FoldbackError> {
    let data = stash.read_channel(&args.ref_id, args.channel, args.offset, args.limit)?;
    out.write_all(&data).map_err(FoldbackError::Io)?;
    Ok(())
}
