use std::path::PathBuf;
use std::process;

use foldback_lib::{
    argv,
    commands::{get, grep, info, purge, tail},
    display::{self, context::CommandContext, registry::Registry},
    error::FoldbackError,
    runner,
    stash::{Channel, SaveArgs, Stash, DEFAULT_TTL_SECS},
};

// ── foldback management exit codes (documented; do not overlap 0–127) ────────
// 0   success
// 1   ref not found / expired
// 2   bad input / invalid ref format
// 3   internal storage / IO error
// Passthrough mode always exits with the child process's exit code.

fn data_dir() -> Result<PathBuf, String> {
    // Priority 1: FOLDBACK_DATA_DIR (full path, used as-is)
    if let Ok(d) = std::env::var("FOLDBACK_DATA_DIR") {
        if !d.is_empty() {
            return Ok(PathBuf::from(d));
        }
    }
    // Priority 2: XDG_DATA_HOME/<foldback>
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("foldback"));
        }
    }
    // Priority 3: HOME/.local/share/foldback
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Ok(PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("foldback"));
        }
    }
    Err("cannot determine data dir: set FOLDBACK_DATA_DIR, XDG_DATA_HOME, or HOME".into())
}

fn main() {
    let raw_args: Vec<String> = std::env::args().collect();

    if raw_args.len() < 2 {
        print_usage();
        process::exit(2);
    }

    match raw_args[1].as_str() {
        "output" => {
            let code = handle_output(&raw_args[2..]);
            process::exit(code);
        }
        "run" => {
            // foldback run -- <cmd> [args...]
            match raw_args.iter().position(|a| a == "--") {
                Some(dash) if dash + 1 < raw_args.len() => {
                    let cmd_args = &raw_args[dash + 1..];
                    let code = handle_run(&cmd_args[0], &cmd_args[1..]);
                    process::exit(code);
                }
                _ => {
                    eprintln!("foldback run: missing -- separator\nUsage: foldback run -- <command> [args...]");
                    process::exit(2);
                }
            }
        }
        _ => {
            // Implicit passthrough: treat all remaining args as the command
            let code = handle_run(&raw_args[1], &raw_args[2..]);
            process::exit(code);
        }
    }
}

/// Execute a child command, stash its output, write condensed display to
/// stdout/stderr. Returns the child's exit code (fail-open on stash errors).
fn handle_run(command: &str, args: &[String]) -> i32 {
    let captured = match runner::capture(command, args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("foldback: failed to execute '{command}': {e}");
            return 127; // command not found convention
        }
    };

    // Attempt stash write; fail-open on any error.
    let stash_result: Option<(String, chrono::DateTime<chrono::Utc>)> = match data_dir() {
        Err(e) => {
            eprintln!("[foldback] stash unavailable (fail-open): {e}");
            None
        }
        Ok(dir) => match Stash::open(&dir) {
            Ok(stash) => {
                match stash.save(SaveArgs {
                    command,
                    args,
                    cwd: &captured.cwd,
                    exit_code: captured.exit_code,
                    stdout: &captured.stdout,
                    stderr: &captured.stderr,
                    ttl_secs: DEFAULT_TTL_SECS,
                }) {
                    Ok(r) => Some(r),
                    Err(e) => {
                        eprintln!("[foldback] stash write failed (fail-open): {e}");
                        None
                    }
                }
            }
            Err(e) => {
                eprintln!("[foldback] stash open failed (fail-open): {e}");
                None
            }
        },
    };

    // Write condensed (or raw) stdout and stderr.
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();

    // Read FOLDBACK_REDUCERS env var: only the exact value "0" disables specialised reducers.
    // Generic head/tail condensing always runs regardless of this flag.
    let reducers_enabled = std::env::var("FOLDBACK_REDUCERS").ok().as_deref() != Some("0");
    let registry = Registry::default_registry();

    match &stash_result {
        Some((ref_id, expires_at)) => {
            let cmd_ctx = CommandContext {
                command: command.to_string(),
                args: args.to_vec(),
                normalized: argv::normalize(command, args),
                exit_code: captured.exit_code,
                cwd: captured.cwd.clone(),
            };
            let (stdout_out, stderr_out) = display::render_passthrough(
                &captured.stdout,
                &captured.stderr,
                &cmd_ctx,
                ref_id,
                expires_at,
                &registry,
                reducers_enabled,
            );
            let mut out = stdout.lock();
            let mut err = stderr.lock();
            write_passthrough_output(&mut out, &mut err, &stdout_out.display, &stderr_out.display);
        }
        None => {
            let mut out = stdout.lock();
            let mut err = stderr.lock();
            write_passthrough_output(&mut out, &mut err, &captured.stdout, &captured.stderr);
        }
    }

    captured.exit_code
}

/// Dispatch `foldback output <subcommand>` management commands.
/// Returns foldback's own exit code (not child's).
fn handle_output(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("foldback output: missing subcommand\nSubcommands: get, tail, grep, info, purge");
        return 2;
    }

    let stash = match data_dir() {
        Err(e) => {
            eprintln!("foldback output: cannot open stash: {e}");
            return 3;
        }
        Ok(dir) => match Stash::open(&dir) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("foldback output: cannot open stash: {e}");
                return 3;
            }
        },
    };

    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let result: Result<(), FoldbackError> = match args[0].as_str() {
        "get" => parse_get(&args[1..], &stash, &mut out),
        "tail" => parse_tail(&args[1..], &stash, &mut out),
        "grep" => parse_grep(&args[1..], &stash, &mut out),
        "info" => parse_info(&args[1..], &stash, &mut out),
        "purge" => {
            if args.get(1).map(|s| s.as_str()) == Some("--expired") {
                purge::run_expired(&stash, &mut out)
            } else {
                eprintln!("foldback output purge: use --expired flag");
                return 2;
            }
        }
        other => {
            eprintln!("foldback output: unknown subcommand '{other}'");
            return 2;
        }
    };

    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("foldback: {e}");
            e.exit_code()
        }
    }
}

// ── argument parsers for management subcommands ──────────────────────────────

fn parse_get(
    args: &[String],
    stash: &Stash,
    out: &mut dyn std::io::Write,
) -> Result<(), FoldbackError> {
    // foldback output get <ref> [--channel stdout|stderr|both] [--offset N] [--limit N]
    if args.is_empty() {
        return Err(FoldbackError::BadInput("get: missing <ref>".into()));
    }
    let ref_id = &args[0];
    let mut channel = Channel::Stdout;
    let mut offset: Option<u64> = None;
    let mut limit: Option<u64> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--channel" => {
                i += 1;
                channel = args
                    .get(i)
                    .ok_or_else(|| FoldbackError::BadInput("--channel: missing value".into()))?
                    .parse()?;
            }
            "--offset" => {
                i += 1;
                offset = Some(parse_u64(args.get(i), "--offset")?);
            }
            "--limit" => {
                i += 1;
                limit = Some(parse_u64(args.get(i), "--limit")?);
            }
            other => {
                return Err(FoldbackError::BadInput(format!(
                    "get: unknown flag '{other}'"
                )));
            }
        }
        i += 1;
    }

    get::run(
        stash,
        &get::GetArgs {
            ref_id: ref_id.clone(),
            channel,
            offset,
            limit,
        },
        out,
    )
}

fn parse_tail(
    args: &[String],
    stash: &Stash,
    out: &mut dyn std::io::Write,
) -> Result<(), FoldbackError> {
    // foldback output tail <ref> [--channel stdout|stderr] [--lines N]
    if args.is_empty() {
        return Err(FoldbackError::BadInput("tail: missing <ref>".into()));
    }
    let ref_id = &args[0];
    let mut channel = Channel::Stdout;
    let mut lines: usize = 10;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--channel" => {
                i += 1;
                channel = args
                    .get(i)
                    .ok_or_else(|| FoldbackError::BadInput("--channel: missing value".into()))?
                    .parse()?;
            }
            "--lines" => {
                i += 1;
                lines = parse_u64(args.get(i), "--lines")? as usize;
            }
            other => {
                return Err(FoldbackError::BadInput(format!(
                    "tail: unknown flag '{other}'"
                )));
            }
        }
        i += 1;
    }

    if channel == Channel::Both {
        return Err(FoldbackError::BadInput(
            "tail --channel: 'both' is not valid; use 'stdout' or 'stderr'".into(),
        ));
    }

    tail::run(
        stash,
        &tail::TailArgs {
            ref_id: ref_id.clone(),
            channel,
            lines,
        },
        out,
    )
}

fn parse_grep(
    args: &[String],
    stash: &Stash,
    out: &mut dyn std::io::Write,
) -> Result<(), FoldbackError> {
    // foldback output grep <ref> <pattern> [--channel stdout|stderr|both]
    if args.len() < 2 {
        return Err(FoldbackError::BadInput(
            "grep: missing <ref> <pattern>".into(),
        ));
    }
    let ref_id = &args[0];
    let pattern = &args[1];
    let mut channel = Channel::Both;

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--channel" => {
                i += 1;
                channel = args
                    .get(i)
                    .ok_or_else(|| FoldbackError::BadInput("--channel: missing value".into()))?
                    .parse()?;
            }
            other => {
                return Err(FoldbackError::BadInput(format!(
                    "grep: unknown flag '{other}'"
                )));
            }
        }
        i += 1;
    }

    grep::run(
        stash,
        &grep::GrepArgs {
            ref_id: ref_id.clone(),
            pattern: pattern.clone(),
            channel,
        },
        out,
    )
}

fn parse_info(
    args: &[String],
    stash: &Stash,
    out: &mut dyn std::io::Write,
) -> Result<(), FoldbackError> {
    let ref_id = args
        .first()
        .ok_or_else(|| FoldbackError::BadInput("info: missing <ref>".into()))?;
    info::run(stash, ref_id, out)
}

fn parse_u64(val: Option<&String>, flag: &str) -> Result<u64, FoldbackError> {
    val.ok_or_else(|| FoldbackError::BadInput(format!("{flag}: missing value")))?
        .parse::<u64>()
        .map_err(|_| FoldbackError::BadInput(format!("{flag}: expected non-negative integer")))
}

fn write_passthrough_output(
    out_writer: &mut dyn std::io::Write,
    err_writer: &mut dyn std::io::Write,
    stdout_data: &[u8],
    stderr_data: &[u8],
) {
    if let Err(e) = out_writer.write_all(stdout_data) {
        let _ = writeln!(err_writer, "[foldback] warning: stdout write failed: {e}");
    }
    if let Err(e) = err_writer.write_all(stderr_data) {
        let _ = writeln!(out_writer, "[foldback] warning: stderr write failed: {e}");
    }
}

fn print_usage() {
    eprintln!(concat!(
        "foldback \u{2014} reversible CLI output capture\n",
        "\n",
        "USAGE:\n",
        "  foldback <command> [args...]               run command (implicit passthrough)\n",
        "  foldback run -- <command> [args...]        explicit escape hatch\n",
        "  foldback output get <ref> [flags]          restore captured output\n",
        "  foldback output tail <ref> [flags]         tail captured output\n",
        "  foldback output grep <ref> <pat> [flags]   grep captured output\n",
        "  foldback output info <ref>                 show ref metadata\n",
        "  foldback output purge --expired            remove expired refs\n",
        "\n",
        "RESERVED namespaces: 'output', 'run' -- use 'foldback run -- ...' to\n",
        "run commands literally named 'output' or 'run'.\n",
        "\n",
        "Environment:\n",
        "  FOLDBACK_DATA_DIR   override storage dir (default: ~/.local/share/foldback)\n",
        "\n",
        "Exit codes (management commands):\n",
        "  0  success\n",
        "  1  ref not found or expired\n",
        "  2  bad input\n",
        "  3  internal storage error\n",
        "\n",
        "Passthrough mode always exits with the child's exit code.\n",
        "Signal exits map to 128+signal (Unix convention).\n",
        "\n",
        "Limitations: no interactive TTY, no watch/server, no Windows.\n",
        "Suitable for bounded-lifetime commands only."
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Write};

    struct FailWriter;
    impl Write for FailWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn write_passthrough_stdout_fail_warns_on_stderr() {
        let mut fail = FailWriter;
        let mut warn_buf: Vec<u8> = Vec::new();
        write_passthrough_output(&mut fail, &mut warn_buf, b"hello", b"");
        let msg = String::from_utf8(warn_buf).unwrap();
        assert!(
            msg.contains("stdout write failed"),
            "expected stdout failure warning in stderr buf, got: {msg:?}"
        );
    }

    #[test]
    fn write_passthrough_stderr_fail_warns_on_stdout() {
        let mut warn_buf: Vec<u8> = Vec::new();
        let mut fail = FailWriter;
        write_passthrough_output(&mut warn_buf, &mut fail, b"", b"world");
        let msg = String::from_utf8(warn_buf).unwrap();
        assert!(
            msg.contains("stderr write failed"),
            "expected stderr failure warning in stdout buf, got: {msg:?}"
        );
    }

    #[test]
    fn write_passthrough_both_fail_does_not_panic() {
        let mut fail1 = FailWriter;
        let mut fail2 = FailWriter;
        write_passthrough_output(&mut fail1, &mut fail2, b"a", b"b");
        // must not panic
    }

    #[test]
    fn write_passthrough_success_passes_data_through() {
        let mut out_buf: Vec<u8> = Vec::new();
        let mut err_buf: Vec<u8> = Vec::new();
        write_passthrough_output(&mut out_buf, &mut err_buf, b"stdout-data", b"stderr-data");
        assert_eq!(out_buf, b"stdout-data");
        assert_eq!(err_buf, b"stderr-data");
    }
}
