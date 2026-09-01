use crate::error::FoldbackError;
use crate::stash::Stash;
use chrono::{TimeZone, Utc};

pub fn run(stash: &Stash, ref_id: &str, out: &mut dyn std::io::Write) -> Result<(), FoldbackError> {
    let meta = stash.meta(ref_id)?;

    let created = Utc
        .timestamp_millis_opt(meta.created_at_ms)
        .single()
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| "?".to_string());

    let expires = Utc
        .timestamp_millis_opt(meta.expires_at_ms)
        .single()
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_else(|| "?".to_string());

    let args_display = meta.args.join(" ");
    writeln!(out, "ref:          {}", meta.ref_id)?;
    writeln!(out, "command:      {} {}", meta.command, args_display)?;
    writeln!(out, "cwd:          {}", meta.cwd)?;
    writeln!(out, "exit_code:    {}", meta.exit_code)?;
    writeln!(out, "created_at:   {created}")?;
    writeln!(out, "expires_at:   {expires}")?;
    writeln!(out, "stdout_size:  {} bytes", meta.stdout_size)?;
    writeln!(out, "stderr_size:  {} bytes", meta.stderr_size)?;
    writeln!(out, "stdout_sha256: {}", meta.stdout_sha256)?;
    writeln!(out, "stderr_sha256: {}", meta.stderr_sha256)?;
    Ok(())
}
