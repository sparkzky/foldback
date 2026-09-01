//! Integration tests — all run against the compiled `foldback` binary.
//!
//! TDD RED phase: these were written before the implementation existed and
//! verified to fail. GREEN phase: full implementation makes them pass.

use assert_cmd::Command;
use tempfile::TempDir;

/// Convenience: build an assert_cmd Command with a fresh isolated data dir.
fn cmd(tmp: &TempDir) -> Command {
    let mut c = Command::cargo_bin("foldback").unwrap();
    c.env("FOLDBACK_DATA_DIR", tmp.path());
    c
}

fn foldback_with(tmp: &TempDir, args: &[&str]) -> assert_cmd::assert::Assert {
    cmd(tmp).args(args).assert()
}

// ── 1. Short output passthrough ─────────────────────────────────────────────

#[test]
fn t01_short_stdout_passthrough() {
    let tmp = TempDir::new().unwrap();
    foldback_with(&tmp, &["echo", "hello"])
        .success()
        .stdout("hello\n");
}

#[test]
fn t01b_short_stderr_passthrough() {
    let tmp = TempDir::new().unwrap();
    foldback_with(&tmp, &["sh", "-c", "echo errout >&2"])
        .success()
        .stderr(predicates::str::contains("errout"));
}

// ── 2. Long output is condensed and produces a ref ──────────────────────────

#[test]
fn t02_long_output_condensed() {
    let tmp = TempDir::new().unwrap();
    // Generate 200 lines (> CONDENSE_LINE_THRESHOLD = 100)
    let assert = foldback_with(&tmp, &["sh", "-c", "seq 1 200"]);
    let out = assert.success().get_output().stdout.clone();
    let s = String::from_utf8_lossy(&out);
    // Must contain the foldback marker
    assert!(
        s.contains("[foldback ref="),
        "expected condensed marker in stdout, got:\n{s}"
    );
    // Must NOT contain all 200 lines (it's condensed)
    assert!(!s.contains("100\n101\n"), "middle lines should be omitted");
}

// ── 3. Full byte-exact recovery via `output get` ────────────────────────────

#[test]
fn t03_byte_exact_recovery() {
    let tmp = TempDir::new().unwrap();

    // Capture a command with known, fixed output
    let out = cmd(&tmp).args(["sh", "-c", "seq 1 200"]).output().unwrap();
    assert!(out.status.success());

    // Extract the ref_id from the condensed stdout
    let condensed = String::from_utf8_lossy(&out.stdout);
    let ref_id = extract_ref_id(&condensed).expect("no ref marker in output");

    // Recover via `foldback output get <ref>`
    let recovered = cmd(&tmp).args(["output", "get", &ref_id]).output().unwrap();
    assert!(recovered.status.success(), "get failed");

    // The recovered bytes must match what `seq 1 200` would produce natively
    let expected: Vec<u8> = (1u32..=200)
        .flat_map(|n| format!("{n}\n").into_bytes())
        .collect();
    assert_eq!(recovered.stdout, expected, "byte-exact recovery failed");
}

// ── 4. stdout / stderr stored in distinct channels ──────────────────────────

#[test]
fn t04_stdout_stderr_distinct() {
    let tmp = TempDir::new().unwrap();

    // Command writes different content to stdout and stderr
    let out = cmd(&tmp)
        .args(["sh", "-c", "seq 1 200; seq 201 400 >&2"])
        .output()
        .unwrap();
    assert!(out.status.success());

    let condensed_stdout = String::from_utf8_lossy(&out.stdout);
    let ref_id = extract_ref_id(&condensed_stdout).expect("no ref in stdout");

    // Recover stdout channel
    let stdout_data = cmd(&tmp)
        .args(["output", "get", &ref_id, "--channel", "stdout"])
        .output()
        .unwrap()
        .stdout;

    // Recover stderr channel
    let stderr_data = cmd(&tmp)
        .args(["output", "get", &ref_id, "--channel", "stderr"])
        .output()
        .unwrap()
        .stdout;

    let stdout_str = String::from_utf8_lossy(&stdout_data);
    let stderr_str = String::from_utf8_lossy(&stderr_data);

    assert!(stdout_str.contains("1\n"), "stdout should contain line 1");
    assert!(
        stdout_str.contains("200\n"),
        "stdout should contain line 200"
    );
    assert!(
        !stdout_str.contains("201\n"),
        "stdout should NOT contain stderr line 201"
    );

    assert!(
        stderr_str.contains("201\n"),
        "stderr should contain line 201"
    );
    assert!(
        !stderr_str.contains("1\n200\n"),
        "stderr should NOT contain stdout lines"
    );
}

// ── 5. Exit code passthrough ─────────────────────────────────────────────────

#[test]
fn t05_exit_code_zero() {
    let tmp = TempDir::new().unwrap();
    foldback_with(&tmp, &["true"]).success();
}

#[test]
fn t05b_exit_code_nonzero() {
    let tmp = TempDir::new().unwrap();
    foldback_with(&tmp, &["sh", "-c", "exit 42"])
        .failure()
        .code(42);
}

#[test]
fn t05c_exit_code_one() {
    let tmp = TempDir::new().unwrap();
    foldback_with(&tmp, &["false"]).failure().code(1);
}

// ── 6. Invalid UTF-8 — binary output handled correctly ──────────────────────

#[test]
fn t06_invalid_utf8_byte_exact() {
    let tmp = TempDir::new().unwrap();
    // Write raw bytes that are not valid UTF-8
    let out = cmd(&tmp)
        .args([
            "python3",
            "-c",
            "import sys; sys.stdout.buffer.write(bytes([0x00,0xff,0xfe,0x80,0x0a]*30))",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());

    // Even if condensed, the stored data must be byte-exact
    let condensed = String::from_utf8_lossy(&out.stdout);
    let ref_id = extract_ref_id(&condensed);

    // If output was short enough not to condense, the raw bytes appear directly
    // If condensed, we need the ref to recover
    let recovered = if let Some(id) = ref_id {
        cmd(&tmp)
            .args(["output", "get", &id])
            .output()
            .unwrap()
            .stdout
    } else {
        out.stdout.clone()
    };

    // Expected: 150 bytes total (5 bytes * 30)
    let expected: Vec<u8> = (0..30)
        .flat_map(|_| vec![0x00u8, 0xff, 0xfe, 0x80, 0x0a])
        .collect();
    assert_eq!(recovered, expected, "binary data not byte-exact");
}

// ── 7. Stash fail-open (read-only data dir) ──────────────────────────────────

#[test]
fn t07_stash_fail_open() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    // Make data dir non-writable so stash will fail
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

    // Should still return the original output, not crash
    let out = cmd(&tmp).args(["echo", "fail-open-test"]).output().unwrap();

    // Must still exit 0 and contain the output
    assert!(
        out.status.success(),
        "foldback must exit 0 even when stash fails"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("fail-open-test")
            || String::from_utf8_lossy(&out.stderr).contains("fail-open-test"),
        "output must be present in fail-open mode"
    );

    // Restore permissions for cleanup
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
}

// ── 8. offset / limit on get ─────────────────────────────────────────────────

#[test]
fn t08_get_offset_limit() {
    let tmp = TempDir::new().unwrap();

    // 200 lines captured and stashed
    let out = cmd(&tmp).args(["sh", "-c", "seq 1 200"]).output().unwrap();
    let condensed = String::from_utf8_lossy(&out.stdout);
    let ref_id = extract_ref_id(&condensed).expect("no ref");

    // Full recovery first
    let full = cmd(&tmp)
        .args(["output", "get", &ref_id])
        .output()
        .unwrap()
        .stdout;

    // Slice: skip first 4 bytes, take 6 bytes  →  "2\n3\n4"  (from "1\n2\n3\n...")
    // "1\n" is 2 bytes, so offset=2 skips "1\n", limit=4 gives "2\n3\n"
    let sliced = cmd(&tmp)
        .args(["output", "get", &ref_id, "--offset", "2", "--limit", "4"])
        .output()
        .unwrap()
        .stdout;

    assert_eq!(&full[2..6], &sliced[..], "offset/limit slice mismatch");
}

// ── 9. tail command ──────────────────────────────────────────────────────────

#[test]
fn t09_tail_command() {
    let tmp = TempDir::new().unwrap();

    let out = cmd(&tmp).args(["sh", "-c", "seq 1 200"]).output().unwrap();
    let condensed = String::from_utf8_lossy(&out.stdout);
    let ref_id = extract_ref_id(&condensed).expect("no ref");

    let tail_out = cmd(&tmp)
        .args(["output", "tail", &ref_id, "--lines", "5"])
        .output()
        .unwrap()
        .stdout;

    let tail_str = String::from_utf8_lossy(&tail_out);
    // Last 5 lines of seq 1 200 are 196..200
    assert!(tail_str.contains("200\n"), "should have line 200");
    assert!(tail_str.contains("196\n"), "should have line 196");
    assert!(!tail_str.contains("195\n"), "should NOT have line 195");
}

// ── 10. grep command ─────────────────────────────────────────────────────────

#[test]
fn t10_grep_command() {
    let tmp = TempDir::new().unwrap();

    let out = cmd(&tmp)
        .args([
            "sh",
            "-c",
            "for i in $(seq 1 200); do echo \"item $i\"; done",
        ])
        .output()
        .unwrap();
    let condensed = String::from_utf8_lossy(&out.stdout);
    let ref_id = extract_ref_id(&condensed).expect("no ref");

    let grep_out = cmd(&tmp)
        .args(["output", "grep", &ref_id, "item 1"])
        .output()
        .unwrap()
        .stdout;

    let grep_str = String::from_utf8_lossy(&grep_out);
    // "item 1", "item 10", "item 11", ..., "item 19", "item 100", ...
    assert!(grep_str.contains("item 1\n"), "should match 'item 1'");
    // Should NOT contain lines that don't have "item 1" as substring
    // "item 2" doesn't contain "item 1" as substring
    assert!(!grep_str.contains("item 2\n"), "should not match 'item 2'");
}

// ── 11. purge expired ────────────────────────────────────────────────────────

#[test]
fn t11_purge_expired() {
    let tmp = TempDir::new().unwrap();

    // Create a ref by running a command
    let out = cmd(&tmp).args(["sh", "-c", "seq 1 200"]).output().unwrap();
    let condensed = String::from_utf8_lossy(&out.stdout);
    let ref_id = extract_ref_id(&condensed).expect("no ref");

    // Force expiry by manipulating DB directly
    {
        use rusqlite::{params, Connection};
        let db_path = tmp.path().join("meta.db");
        let db = Connection::open(db_path).unwrap();
        db.execute(
            "UPDATE refs SET expires_at = 1 WHERE ref_id = ?1",
            params![ref_id],
        )
        .unwrap();
    }

    // Now get should return exit 1 (expired)
    foldback_with(&tmp, &["output", "get", &ref_id])
        .failure()
        .code(1);

    // purge --expired should succeed and report 1 purged
    let purge_out = foldback_with(&tmp, &["output", "purge", "--expired"])
        .success()
        .get_output()
        .stdout
        .clone();

    let purge_str = String::from_utf8_lossy(&purge_out);
    assert!(purge_str.contains("1"), "expected '1' purged ref in output");
}

// ── 12. info command ─────────────────────────────────────────────────────────

#[test]
fn t12_info_command() {
    let tmp = TempDir::new().unwrap();

    let out = cmd(&tmp).args(["sh", "-c", "seq 1 200"]).output().unwrap();
    let condensed = String::from_utf8_lossy(&out.stdout);
    let ref_id = extract_ref_id(&condensed).expect("no ref");

    let info = cmd(&tmp)
        .args(["output", "info", &ref_id])
        .output()
        .unwrap()
        .stdout;

    let info_str = String::from_utf8_lossy(&info);
    assert!(info_str.contains(&ref_id), "info should show ref_id");
    assert!(
        info_str.contains("exit_code:"),
        "info should show exit_code"
    );
    assert!(
        info_str.contains("stdout_sha256:"),
        "info should show sha256"
    );
    assert!(info_str.contains("expires_at:"), "info should show expiry");
}

// ── 13. Concurrent refs do not collide data ──────────────────────────────────

#[test]
fn t13_concurrent_refs_no_collision() {
    use std::thread;

    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().to_path_buf();

    let handles: Vec<_> = (0..6)
        .map(|i| {
            let d = dir.clone();
            thread::spawn(move || {
                let mut c = Command::cargo_bin("foldback").unwrap();
                c.env("FOLDBACK_DATA_DIR", &d);
                let out = c
                    .args([
                        "sh",
                        "-c",
                        &format!(
                            "seq {start} {end}",
                            start = i * 200 + 1,
                            end = i * 200 + 200
                        ),
                    ])
                    .output()
                    .unwrap();
                let condensed = String::from_utf8_lossy(&out.stdout).to_string();
                let ref_id = extract_ref_id(&condensed).expect("no ref in concurrent test");
                (ref_id, i as u32)
            })
        })
        .collect();

    let results: Vec<(String, u32)> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    // All ref_ids must be unique
    let ids: std::collections::HashSet<_> = results.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids.len(), 6, "ref_id collision among concurrent captures");

    // Each ref must recover the correct data for its thread
    for (ref_id, i) in &results {
        let mut c = Command::cargo_bin("foldback").unwrap();
        c.env("FOLDBACK_DATA_DIR", tmp.path());
        let recovered = c.args(["output", "get", ref_id]).output().unwrap().stdout;

        let start = i * 200 + 1;
        let end = i * 200 + 200;
        let expected: Vec<u8> = (start..=end)
            .flat_map(|n| format!("{n}\n").into_bytes())
            .collect();
        assert_eq!(
            recovered, expected,
            "data mismatch for ref {ref_id} (thread {i})"
        );
    }
}

// ── 14. Invalid ref format returns exit 2 ────────────────────────────────────

#[test]
fn t14_invalid_ref_format() {
    let tmp = TempDir::new().unwrap();
    foldback_with(&tmp, &["output", "get", "not-a-valid-ref"])
        .failure()
        .code(2);
}

// ── 15. Not-found ref returns exit 1 ─────────────────────────────────────────

#[test]
fn t15_not_found_ref() {
    let tmp = TempDir::new().unwrap();
    foldback_with(&tmp, &["output", "get", "aabbccddeeff00112233445566778899"])
        .failure()
        .code(1);
}

// ── 16. `foldback run -- <cmd>` explicit escape hatch ────────────────────────

#[test]
fn t16_explicit_run_escape_hatch() {
    let tmp = TempDir::new().unwrap();
    foldback_with(&tmp, &["run", "--", "echo", "explicit"])
        .success()
        .stdout(predicates::str::contains("explicit"));
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Parse `ref=<ref_id>` from a condensed foldback marker line.
fn extract_ref_id(s: &str) -> Option<String> {
    for chunk in s.split("ref=") {
        let candidate: String = chunk.chars().take(32).collect();
        if candidate.len() == 32 && candidate.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(candidate);
        }
    }
    None
}
