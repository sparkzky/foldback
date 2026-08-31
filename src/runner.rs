use std::process::Command;
use std::time::Instant;

pub struct CaptureResult {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: i32,
    pub cwd: String,
    pub command: String,
    pub args: Vec<String>,
    pub started_at_ms: i64,
    pub duration_ms: u64,
}

pub fn capture(command: &str, args: &[String]) -> std::io::Result<CaptureResult> {
    let cwd = std::env::current_dir()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();

    let started_at_ms = chrono::Utc::now().timestamp_millis();
    let t0 = Instant::now();

    let output = Command::new(command).args(args).output()?;

    let duration_ms = t0.elapsed().as_millis() as u64;
    let exit_code = exit_code_from_status(&output.status);

    Ok(CaptureResult {
        stdout: output.stdout,
        stderr: output.stderr,
        exit_code,
        cwd,
        command: command.to_string(),
        args: args.to_vec(),
        started_at_ms,
        duration_ms,
    })
}

fn exit_code_from_status(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    // Signal-killed process: 128 + signal, or 128 if unknown
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    128
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capture_exit_zero() {
        let res = capture("true", &[]).unwrap();
        assert_eq!(res.exit_code, 0);
        assert!(res.stdout.is_empty());
    }

    #[test]
    fn test_capture_exit_nonzero() {
        let res = capture("false", &[]).unwrap();
        assert_eq!(res.exit_code, 1);
    }

    #[test]
    fn test_capture_stdout() {
        let res = capture("echo", &["hello".to_string()]).unwrap();
        assert_eq!(res.exit_code, 0);
        assert_eq!(res.stdout, b"hello\n");
    }

    #[test]
    fn test_capture_stderr() {
        let res = capture("sh", &["-c".to_string(), "echo err >&2".to_string()]).unwrap();
        assert_eq!(res.exit_code, 0);
        assert!(res.stdout.is_empty());
        assert_eq!(res.stderr, b"err\n");
    }

    #[test]
    fn test_capture_binary_stdout() {
        let res = capture(
            "python3",
            &[
                "-c".to_string(),
                "import sys; sys.stdout.buffer.write(bytes([0,1,2,255,254,128]))".to_string(),
            ],
        )
        .unwrap();
        assert_eq!(res.exit_code, 0);
        assert_eq!(res.stdout, &[0u8, 1, 2, 255, 254, 128]);
    }
}
