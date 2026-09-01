use crate::error::FoldbackError;
use crate::stash::{Channel, Stash};

pub struct GrepArgs {
    pub ref_id: String,
    pub pattern: String,
    pub channel: Channel,
}

pub fn run(
    stash: &Stash,
    args: &GrepArgs,
    out: &mut dyn std::io::Write,
) -> Result<(), FoldbackError> {
    let data = stash.grep_lines(&args.ref_id, args.channel, &args.pattern)?;
    out.write_all(&data).map_err(FoldbackError::Io)?;
    Ok(())
}
