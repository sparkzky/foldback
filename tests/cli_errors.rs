//! Black-box tests for `foldback output` management command error paths.
//!
//! Covers dispatch-level errors (missing/unknown subcommand, purge flags) and
//! subcommand argument/lookup failures with stable exit-code semantics.

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use tempfile::TempDir;

const NOT_FOUND_REF: &str = "aabbccddeeff00112233445566778899";

fn cmd(tmp: &TempDir) -> Command {
    let mut c = Command::cargo_bin("foldback").unwrap();
    c.env("FOLDBACK_DATA_DIR", tmp.path());
    c
}

fn foldback_with(tmp: &TempDir, args: &[&str]) -> assert_cmd::assert::Assert {
    cmd(tmp).args(args).assert()
}

fn create_ref(tmp: &TempDir) -> String {
    let out = cmd(tmp).args(["sh", "-c", "seq 1 200"]).output().unwrap();
    assert!(out.status.success(), "capture failed");
    let condensed = String::from_utf8_lossy(&out.stdout);
    extract_ref_id(&condensed).expect("no ref marker in capture output")
}

fn expire_ref(tmp: &TempDir, ref_id: &str) {
    use rusqlite::{params, Connection};
    let db_path = tmp.path().join("meta.db");
    let db = Connection::open(db_path).unwrap();
    db.execute(
        "UPDATE refs SET expires_at = 1 WHERE ref_id = ?1",
        params![ref_id],
    )
    .unwrap();
}

fn extract_ref_id(s: &str) -> Option<String> {
    for chunk in s.split("ref=") {
        let candidate: String = chunk.chars().take(32).collect();
        if candidate.len() == 32 && candidate.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(candidate);
        }
    }
    None
}

// ── dispatch errors ───────────────────────────────────────────────────────────

#[test]
fn e01_output_missing_subcommand() {
    let tmp = TempDir::new().unwrap();
    foldback_with(&tmp, &["output"])
        .failure()
        .code(2)
        .stderr(predicates::str::contains("missing subcommand"))
        .stderr(predicates::str::contains("Subcommands:"));
}

#[test]
fn e02_output_unknown_subcommand() {
    let tmp = TempDir::new().unwrap();
    foldback_with(&tmp, &["output", "nosuch"])
        .failure()
        .code(2)
        .stderr(predicates::str::contains("unknown subcommand"));
}

#[test]
fn e03_purge_missing_expired_flag() {
    let tmp = TempDir::new().unwrap();
    foldback_with(&tmp, &["output", "purge"])
        .failure()
        .code(2)
        .stderr(predicates::str::contains("--expired"));
}

// ── flag / argument parse errors ──────────────────────────────────────────────

#[test]
fn e04_get_bad_channel() {
    let tmp = TempDir::new().unwrap();
    foldback_with(&tmp, &["output", "get", NOT_FOUND_REF, "--channel", "bad"])
        .failure()
        .code(2)
        .stderr(predicates::str::contains("unknown channel"));
}

#[test]
fn e05_get_missing_offset_value() {
    let tmp = TempDir::new().unwrap();
    foldback_with(&tmp, &["output", "get", NOT_FOUND_REF, "--offset"])
        .failure()
        .code(2)
        .stderr(predicates::str::contains("--offset"))
        .stderr(predicates::str::contains("missing value"));
}

#[test]
fn e06_get_missing_limit_value() {
    let tmp = TempDir::new().unwrap();
    foldback_with(&tmp, &["output", "get", NOT_FOUND_REF, "--limit"])
        .failure()
        .code(2)
        .stderr(predicates::str::contains("--limit"))
        .stderr(predicates::str::contains("missing value"));
}

// ── ref lookup errors (valid format, absent from stash) ───────────────────────

#[test]
fn e07_not_found_ref_get() {
    let tmp = TempDir::new().unwrap();
    foldback_with(&tmp, &["output", "get", NOT_FOUND_REF])
        .failure()
        .code(1)
        .stderr(predicates::str::contains("not found"));
}

#[test]
fn e08_not_found_ref_tail() {
    let tmp = TempDir::new().unwrap();
    foldback_with(&tmp, &["output", "tail", NOT_FOUND_REF])
        .failure()
        .code(1)
        .stderr(predicates::str::contains("not found"));
}

#[test]
fn e09_not_found_ref_grep() {
    let tmp = TempDir::new().unwrap();
    foldback_with(&tmp, &["output", "grep", NOT_FOUND_REF, "pat"])
        .failure()
        .code(1)
        .stderr(predicates::str::contains("not found"));
}

#[test]
fn e10_not_found_ref_info() {
    let tmp = TempDir::new().unwrap();
    foldback_with(&tmp, &["output", "info", NOT_FOUND_REF])
        .failure()
        .code(1)
        .stderr(predicates::str::contains("not found"));
}

// ── expired ref: unified exit 1 across read subcommands ───────────────────────

#[test]
fn e11_expired_ref_get() {
    let tmp = TempDir::new().unwrap();
    let ref_id = create_ref(&tmp);
    expire_ref(&tmp, &ref_id);

    foldback_with(&tmp, &["output", "get", &ref_id])
        .failure()
        .code(1)
        .stderr(predicates::str::contains("expired"));
}

#[test]
fn e12_expired_ref_tail() {
    let tmp = TempDir::new().unwrap();
    let ref_id = create_ref(&tmp);
    expire_ref(&tmp, &ref_id);

    foldback_with(&tmp, &["output", "tail", &ref_id])
        .failure()
        .code(1)
        .stderr(predicates::str::contains("expired"));
}

#[test]
fn e13_expired_ref_grep() {
    let tmp = TempDir::new().unwrap();
    let ref_id = create_ref(&tmp);
    expire_ref(&tmp, &ref_id);

    foldback_with(&tmp, &["output", "grep", &ref_id, "1"])
        .failure()
        .code(1)
        .stderr(predicates::str::contains("expired"));
}

#[test]
fn e14_expired_ref_info() {
    let tmp = TempDir::new().unwrap();
    let ref_id = create_ref(&tmp);
    expire_ref(&tmp, &ref_id);

    foldback_with(&tmp, &["output", "info", &ref_id])
        .failure()
        .code(1)
        .stderr(predicates::str::contains("expired"));
}

// ── data_dir: no /tmp fallback when HOME/XDG/FOLDBACK_DATA_DIR all absent ────

/// Build a Command with FOLDBACK_DATA_DIR, XDG_DATA_HOME, and HOME all removed.
/// foldback must not silently fall back to /tmp for the data directory.
fn foldback_no_data_dir(args: &[&str]) -> assert_cmd::assert::Assert {
    Command::cargo_bin("foldback")
        .unwrap()
        .env_remove("FOLDBACK_DATA_DIR")
        .env_remove("XDG_DATA_HOME")
        .env_remove("HOME")
        .args(args)
        .assert()
}

#[test]
fn e15_no_data_dir_management_exits_3() {
    // When no env-var provides a data directory, management commands must exit 3.
    foldback_no_data_dir(&["output", "info", NOT_FOUND_REF])
        .failure()
        .code(3)
        .stderr(
            predicates::str::contains("data dir")
                .or(predicates::str::contains("FOLDBACK_DATA_DIR")),
        );
}

#[test]
fn e16_no_data_dir_passthrough_exits_child_code() {
    // Passthrough must be fail-open: stash is skipped, child exit code is preserved.
    foldback_no_data_dir(&["sh", "-c", "exit 7"])
        .failure()
        .code(7);
}

// ── tail: --channel both must be rejected (exit 2) ───────────────────────────

#[test]
fn e17_tail_rejects_channel_both() {
    let tmp = TempDir::new().unwrap();
    foldback_with(
        &tmp,
        &["output", "tail", NOT_FOUND_REF, "--channel", "both"],
    )
    .failure()
    .code(2)
    .stderr(predicates::str::contains("both").or(predicates::str::contains("bad input")));
}
