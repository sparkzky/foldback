use crate::error::FoldbackError;
use crate::stash::Stash;

pub fn run_expired(stash: &Stash, out: &mut dyn std::io::Write) -> Result<(), FoldbackError> {
    let n = stash.purge_expired()?;
    writeln!(out, "purged {n} expired ref(s)")?;
    Ok(())
}
