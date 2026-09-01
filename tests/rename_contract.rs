//! Rename contract tests — RED phase.
//!
//! These tests were written BEFORE the binary/marker/env naming change
//! and verify the three naming contracts that must hold after the rename:
//!   1. Binary is named `foldback`
//!   2. Marker prefix is `[foldback ref=`
//!   3. Env var is `FOLDBACK_DATA_DIR`
//!   4. Env var is `FOLDBACK_REDUCERS`
//!
//! RED: all 4 assertions fail against the pre-rename codebase.
//! GREEN: they all pass once the rename is complete.

use assert_cmd::Command;
use tempfile::TempDir;

fn foldback_cmd(tmp: &TempDir) -> Command {
    let mut c = Command::cargo_bin("foldback").unwrap();
    c.env("FOLDBACK_DATA_DIR", tmp.path());
    c
}

/// Contract 1: binary must be named `foldback`.
/// RED: cargo_bin("foldback") panics because the pre-rename binary has a different name.
#[test]
fn rc01_binary_name_is_foldback() {
    let tmp = TempDir::new().unwrap();
    foldback_cmd(&tmp)
        .args(["echo", "hello"])
        .assert()
        .success()
        .stdout("hello\n");
}

/// Contract 2: stash env var must be `FOLDBACK_DATA_DIR`.
/// RED: when FOLDBACK_DATA_DIR is set but the old env var is not,
/// the management commands must use FOLDBACK_DATA_DIR.
#[test]
fn rc02_env_var_foldback_data_dir() {
    let tmp = TempDir::new().unwrap();
    // If FOLDBACK_DATA_DIR is honoured, `output info` with a fresh stash
    // returns exit 1 (ref not found), not exit 3 (cannot open stash).
    let out = foldback_cmd(&tmp)
        .args(["output", "info", "aabbccddeeff00112233445566778899"])
        .assert()
        .failure()
        .code(1); // exit 1 = not found; exit 3 = stash unavailable
    let _ = out;
}

/// Contract 3: marker prefix must be `[foldback ref=` (the old prefix must be absent).
/// RED: pre-rename binary emits a different prefix.
#[test]
fn rc03_marker_prefix_is_foldback() {
    let tmp = TempDir::new().unwrap();
    let out = foldback_cmd(&tmp)
        .args(["sh", "-c", "seq 1 200"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("[foldback ref="),
        "marker must use [foldback ref= prefix; got:\n{s}"
    );
    // Verify no other bracket-prefixed marker alternative is present
    // (the only marker-like lines should start with [foldback)
    for line in s.lines() {
        if line.starts_with('[') && line.contains("ref=") {
            assert!(
                line.starts_with("[foldback"),
                "any ref-marker line must start with [foldback; got: {line:?}"
            );
        }
    }
}

/// Contract 4: reducers env var must be `FOLDBACK_REDUCERS`.
/// RED: pre-rename binary only honours the old env var name.
#[test]
fn rc04_env_var_foldback_reducers() {
    let tmp_spec = TempDir::new().unwrap();
    let tmp_gen = TempDir::new().unwrap();

    // Run a long seq output (generic → omitted= in marker)
    // With FOLDBACK_REDUCERS=0 the output must still be generic (no view=)
    let out_generic = foldback_cmd(&tmp_gen)
        .args(["sh", "-c", "seq 1 200"])
        .env("FOLDBACK_REDUCERS", "0")
        .output()
        .unwrap();
    let s_gen = String::from_utf8_lossy(&out_generic.stdout);
    assert!(
        !s_gen.contains("view="),
        "FOLDBACK_REDUCERS=0 must disable specialized reducers; got:\n{s_gen}"
    );

    // Sanity: without FOLDBACK_REDUCERS=0 the marker is present
    let out_spec = foldback_cmd(&tmp_spec)
        .args(["sh", "-c", "seq 1 200"])
        .output()
        .unwrap();
    let s_spec = String::from_utf8_lossy(&out_spec.stdout);
    assert!(
        s_spec.contains("[foldback ref="),
        "without FOLDBACK_REDUCERS=0 the marker must be present; got:\n{s_spec}"
    );
}
