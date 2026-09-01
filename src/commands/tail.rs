use crate::error::FoldbackError;
use crate::stash::{Channel, Stash};

pub struct TailArgs {
    pub ref_id: String,
    pub channel: Channel,
    pub lines: usize,
}

pub fn run(
    stash: &Stash,
    args: &TailArgs,
    out: &mut dyn std::io::Write,
) -> Result<(), FoldbackError> {
    let data = stash.tail_lines(&args.ref_id, args.channel, args.lines)?;
    out.write_all(&data).map_err(FoldbackError::Io)?;
    Ok(())
}
