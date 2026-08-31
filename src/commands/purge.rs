use crate::error::RawrefError;
use crate::stash::Stash;

pub fn run_expired(stash: &Stash, out: &mut dyn std::io::Write) -> Result<(), RawrefError> {
    let n = stash.purge_expired()?;
    writeln!(out, "purged {n} expired ref(s)")?;
    Ok(())
}
