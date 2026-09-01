//! Phase 2 E2E integration tests — black-box coverage of the full pipeline.
//!
//! Each test creates isolated TempDirs for RAWREF_DATA_DIR and a fake-bin PATH.
//! Fake `pytest`, `cargo`, and `python3` scripts output >100-line real-style content
//! and write a counter file to prove single child execution.
//!
//! All tests can run in parallel (no shared state; each test owns its TempDirs).
//!
//! Coverage:
//!   p01  generic seq behaviour equivalent to Phase 1
//!   p02  pytest specialized marker + byte-exact get + single execution
//!   p02b python3 -m pytest argv routing
//!   p03  cargo test specialized marker + byte-exact get + single execution
//!   p04  never-worse: display strictly smaller than raw (including marker bytes)
//!   p05  malformed/parsefail → generic, no view=
//!   p06  non-UTF-8 → generic/raw; output get byte-exact
//!   p07  short output → raw passthrough, no marker
//!   p08  --collect-only gate → generic (no view=pytest)
//!   p08b --message-format=json gate → generic (no view=cargo-test)
//!   p09  RAWREF_REDUCERS=0 → generic, no view=
//!   p10  unmatched command → generic, no view=
//!   p11  stash fail-open → raw output, no marker, child exit preserved
//!   p12  pytest failure exit code passthrough
//!   p12b cargo test failure exit code passthrough, view=cargo-test present
//!   p13  marker contract: generic omitted=; specialized recoverability=retrievable, no omitted=
//!   p14  cargo stderr compile errors → no view=cargo-test on stderr
//!   p15  successful pytest → no FAILED text in display

use assert_cmd::Command;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

// ── helpers ───────────────────────────────────────────────────────────────────

/// rawref Command with isolated data dir and prepended fake-bin in PATH.
fn rawref_cmd(data_dir: &TempDir, bin_dir: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("rawref").unwrap();
    c.env("RAWREF_DATA_DIR", data_dir.path());
    let orig = std::env::var("PATH").unwrap_or_default();
    c.env("PATH", format!("{}:{}", bin_dir.display(), orig));
    c
}

/// Extract the first 32-hex ref_id from a rawref marker line.
/// Works for both generic and specialized markers (prefix `ref=` is shared).
fn extract_ref_id(s: &str) -> Option<String> {
    for chunk in s.split("ref=") {
        let candidate: String = chunk.chars().take(32).collect();
        if candidate.len() == 32 && candidate.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(candidate);
        }
    }
    None
}

/// Write a shell script to `path` with mode 0700.
fn write_script(path: &std::path::Path, content: &str) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

/// Count execution records in a counter file (each call appends one line).
fn read_exec_count(counter_path: &std::path::Path) -> usize {
    if !counter_path.exists() {
        return 0;
    }
    fs::read_to_string(counter_path).unwrap().lines().count()
}

/// Generate >100-line realistic pytest output (all passing, no failures).
///
/// Produces: 5-line header + blank + N PASSED lines + blank + final summary.
/// N must be > 100 to ensure the line-count threshold is exceeded.
fn pytest_passing_fixture(n_tests: usize) -> String {
    assert!(n_tests > 100, "n_tests must exceed 100-line threshold");
    let mut s = String::new();
    s.push_str(
        "============================= test session starts ==============================\n",
    );
    s.push_str("platform linux -- Python 3.11.0, pytest-7.4.0, pluggy-1.3.0\n");
    s.push_str("rootdir: /home/user/project\n");
    s.push_str(&format!("collected {n_tests} items\n"));
    s.push('\n');
    for i in 1..=n_tests {
        let pct = i * 100 / n_tests;
        s.push_str(&format!(
            "tests/test_unit.py::test_fn_{i:03} PASSED                         [{pct:3}%]\n"
        ));
    }
    s.push('\n');
    s.push_str(&format!(
        "============================== {n_tests} passed in 2.34s ==============================\n"
    ));
    s
}

/// Generate >100-line realistic pytest output with one failing test.
///
/// Layout: 5-line header + N PASSED + 1 FAILED + FAILURES section +
///         short test summary + final "N failed, M passed" summary.
fn pytest_failing_fixture(n_pass: usize) -> String {
    assert!(n_pass > 100, "n_pass must exceed 100-line threshold");
    let mut s = String::new();
    s.push_str(
        "============================= test session starts ==============================\n",
    );
    s.push_str("platform linux -- Python 3.11.0, pytest-7.4.0, pluggy-1.3.0\n");
    s.push_str("rootdir: /home/user/project\n");
    s.push_str(&format!("collected {} items\n", n_pass + 1));
    s.push('\n');
    for i in 1..=n_pass {
        let pct = i * 100 / (n_pass + 1);
        s.push_str(&format!(
            "tests/test_unit.py::test_fn_{i:03} PASSED                         [{pct:3}%]\n"
        ));
    }
    s.push_str("tests/test_unit.py::test_fn_bad FAILED                          [100%]\n");
    s.push('\n');
    s.push_str(
        "=================================== FAILURES ===================================\n",
    );
    s.push_str("__________________________ test_fn_bad ________________________________________\n");
    s.push('\n');
    s.push_str("    def test_fn_bad():\n");
    s.push_str(">       assert False\n");
    s.push_str("E       AssertionError: assert False\n");
    s.push('\n');
    s.push_str("tests/test_unit.py:5: AssertionError\n");
    s.push_str(
        "=========================== short test summary info ============================\n",
    );
    s.push_str("FAILED tests/test_unit.py::test_fn_bad - AssertionError: assert False\n");
    s.push_str(&format!(
        "============================== 1 failed, {n_pass} passed in 2.34s ==============================\n"
    ));
    s
}

/// Generate >100-line realistic cargo test output (all passing).
///
/// Layout: `running N tests` + N `... ok` lines + blank + summary.
fn cargo_passing_fixture(n_tests: usize) -> String {
    assert!(n_tests > 100, "n_tests must exceed 100-line threshold");
    let mut s = String::new();
    s.push_str(&format!("running {n_tests} tests\n"));
    for i in 1..=n_tests {
        s.push_str(&format!("test tests::test_fn_{i:04} ... ok\n"));
    }
    s.push('\n');
    s.push_str(&format!(
        "test result: ok. {n_tests} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s\n"
    ));
    s
}

/// Generate >100-line realistic cargo test output with one failing test.
fn cargo_failing_fixture(n_ok: usize) -> String {
    assert!(n_ok > 100, "n_ok must exceed 100-line threshold");
    let mut s = String::new();
    s.push_str(&format!("running {} tests\n", n_ok + 1));
    for i in 1..=n_ok {
        s.push_str(&format!("test tests::test_fn_{i:04} ... ok\n"));
    }
    s.push_str("test tests::test_the_failure ... FAILED\n");
    s.push('\n');
    s.push_str("failures:\n\n");
    s.push_str("---- tests::test_the_failure stdout ----\n");
    s.push_str("thread 'tests::test_the_failure' panicked at 'assertion `left == right` failed\n");
    s.push_str("  left: 2\n right: 3'\n");
    s.push_str("note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\n");
    s.push('\n');
    s.push_str("failures:\n");
    s.push_str("    tests::test_the_failure\n\n");
    s.push_str(&format!(
        "test result: FAILED. {n_ok} passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s\n"
    ));
    s
}

/// Write a fake `pytest` script to `script_path`.
///
/// The script:
///   1. Appends "x\n" to `counter_file` (execution proof).
///   2. Cats `fixture_file` to stdout.
///   3. Exits with `exit_code`.
fn write_pytest_script(
    script_path: &std::path::Path,
    fixture_file: &std::path::Path,
    counter_file: &std::path::Path,
    exit_code: i32,
) {
    let script = format!(
        "#!/bin/sh\necho x >> {counter}\ncat {fixture}\nexit {code}\n",
        counter = counter_file.display(),
        fixture = fixture_file.display(),
        code = exit_code,
    );
    write_script(script_path, &script);
}

/// Write a fake `cargo` script to `script_path`.
///
/// The script:
///   1. Asserts first arg is "test" (exits 1 otherwise).
///   2. Appends "x\n" to `counter_file`.
///   3. Cats `fixture_file` to stdout.
///   4. Exits with `exit_code`.
fn write_cargo_script(
    script_path: &std::path::Path,
    fixture_file: &std::path::Path,
    counter_file: &std::path::Path,
    exit_code: i32,
) {
    let script = format!(
        concat!(
            "#!/bin/sh\n",
            "if [ \"$1\" != \"test\" ]; then\n",
            "  echo 'cargo: requires test subcommand' >&2\n",
            "  exit 1\n",
            "fi\n",
            "echo x >> {counter}\n",
            "cat {fixture}\n",
            "exit {code}\n",
        ),
        counter = counter_file.display(),
        fixture = fixture_file.display(),
        code = exit_code,
    );
    write_script(script_path, &script);
}

// ─────────────────────────────────────────────────────────────────────────────
// p01 — generic command long output → generic marker, no view=, has omitted=
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn p01_generic_long_output_generic_marker() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();

    // `sh -c "seq 1 200"` is an unmatched command → Generic normalisation
    let out = rawref_cmd(&tmp, bin.path())
        .args(["sh", "-c", "seq 1 200"])
        .output()
        .unwrap();

    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);

    assert!(s.contains("[rawref ref="), "must contain rawref marker");
    assert!(
        !s.contains("view="),
        "generic marker must NOT contain view="
    );
    assert!(
        s.contains("omitted="),
        "generic marker must contain omitted="
    );
    // Middle lines must be condensed away
    assert!(!s.contains("100\n101"), "middle lines must be omitted");
    // Head/tail lines must be present
    assert!(s.contains("1\n"), "first lines must be present");
    assert!(s.contains("200\n"), "last lines must be present");
}

// ─────────────────────────────────────────────────────────────────────────────
// p02 — pytest specialized marker + byte-exact get + single child execution
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn p02_pytest_specialized_byte_exact_single_exec() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();

    let content = pytest_passing_fixture(110);
    let fixture_path = tmp.path().join("pytest_fixture.txt");
    fs::write(&fixture_path, &content).unwrap();
    let counter_path = tmp.path().join("counter_pytest");
    write_pytest_script(&bin.path().join("pytest"), &fixture_path, &counter_path, 0);

    // Run rawref pytest (>100 lines → threshold exceeded → specialized reducer)
    let out = rawref_cmd(&tmp, bin.path()).arg("pytest").output().unwrap();

    assert!(out.status.success(), "pytest exit 0 must propagate");
    let s = String::from_utf8_lossy(&out.stdout);

    // Specialized marker fields
    assert!(s.contains("[rawref ref="), "must contain rawref marker");
    assert!(s.contains("view=pytest"), "must contain view=pytest");
    assert!(
        s.contains("recoverability=retrievable"),
        "must contain recoverability=retrievable"
    );
    assert!(
        !s.contains("omitted="),
        "specialized marker must NOT contain omitted="
    );

    // Marker must be on its own line (byte immediately before "[rawref" must be '\n')
    let marker_pos = s.find("[rawref ref=").expect("marker must be present");
    assert!(marker_pos > 0, "marker must not be the very first byte");
    assert_eq!(
        out.stdout[marker_pos - 1],
        b'\n',
        "marker must be preceded by '\\n' (own line)"
    );

    // Noise must be stripped by the pytest reducer
    assert!(
        !s.contains("PASSED"),
        "PASSED progress lines must be removed"
    );
    assert!(
        !s.contains("FAILED"),
        "success path must not contain FAILED"
    );
    assert!(
        !s.contains("platform linux"),
        "platform header must be stripped"
    );

    // Final summary must be preserved
    assert!(
        s.contains("110 passed in 2.34s"),
        "final summary must be preserved"
    );

    // Single-execution proof: counter must show exactly 1 invocation
    assert_eq!(
        read_exec_count(&counter_path),
        1,
        "pytest must be executed exactly once"
    );

    // Byte-exact recovery via `output get`
    let ref_id = extract_ref_id(&s).expect("must have ref_id in marker");
    let recovered = rawref_cmd(&tmp, bin.path())
        .args(["output", "get", &ref_id])
        .output()
        .unwrap();
    assert!(recovered.status.success(), "output get must succeed");
    assert_eq!(
        recovered.stdout,
        content.as_bytes(),
        "output get must return byte-exact raw content"
    );

    // `output get` must NOT re-execute pytest
    assert_eq!(
        read_exec_count(&counter_path),
        1,
        "output get must NOT re-execute pytest"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// p02b — python3 -m pytest routing → view=pytest (argv verification)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn p02b_python3_m_pytest_routing() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();

    let content = pytest_passing_fixture(110);
    let fixture_path = tmp.path().join("py3_fixture.txt");
    fs::write(&fixture_path, &content).unwrap();
    let counter_path = tmp.path().join("counter_python3");

    // python3 script: verifies argv[1]="-m" and argv[2]="pytest" before acting
    let script = format!(
        concat!(
            "#!/bin/sh\n",
            "# Verify argv: must be called as `python3 -m pytest`\n",
            "if [ \"$1\" = \"-m\" ] && [ \"$2\" = \"pytest\" ]; then\n",
            "  echo x >> {counter}\n",
            "  cat {fixture}\n",
            "  exit 0\n",
            "fi\n",
            "# Not a pytest invocation — exit non-zero to signal misuse\n",
            "exit 2\n",
        ),
        counter = counter_path.display(),
        fixture = fixture_path.display(),
    );
    write_script(&bin.path().join("python3"), &script);

    let out = rawref_cmd(&tmp, bin.path())
        .args(["python3", "-m", "pytest"])
        .output()
        .unwrap();

    assert!(out.status.success(), "python3 -m pytest must succeed");
    let s = String::from_utf8_lossy(&out.stdout);

    // argv normalize: basename "python3" + "-m pytest" → Pytest { module_invocation: true }
    assert!(
        s.contains("view=pytest"),
        "python3 -m pytest must use pytest view"
    );
    assert!(
        s.contains("recoverability=retrievable"),
        "must contain recoverability=retrievable"
    );
    assert!(
        !s.contains("omitted="),
        "specialized must not have omitted="
    );
    assert_eq!(read_exec_count(&counter_path), 1, "single execution");
}

// ─────────────────────────────────────────────────────────────────────────────
// p03 — cargo test specialized marker + byte-exact get + single execution
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn p03_cargo_test_specialized_byte_exact_single_exec() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();

    // >100 lines: threshold is 100 lines; cargo_passing_fixture(105) = 108 lines
    let content = cargo_passing_fixture(105);
    let fixture_path = tmp.path().join("cargo_fixture.txt");
    fs::write(&fixture_path, &content).unwrap();
    let counter_path = tmp.path().join("counter_cargo");
    write_cargo_script(&bin.path().join("cargo"), &fixture_path, &counter_path, 0);

    let out = rawref_cmd(&tmp, bin.path())
        .args(["cargo", "test"])
        .output()
        .unwrap();

    assert!(out.status.success(), "cargo test exit 0 must propagate");
    let s = String::from_utf8_lossy(&out.stdout);

    // Specialized marker fields
    assert!(s.contains("[rawref ref="), "must contain rawref marker");
    assert!(
        s.contains("view=cargo-test"),
        "must contain view=cargo-test"
    );
    assert!(
        s.contains("recoverability=retrievable"),
        "must contain recoverability=retrievable"
    );
    assert!(
        !s.contains("omitted="),
        "specialized marker must NOT contain omitted="
    );

    // Marker must be on its own line
    let marker_pos = s.find("[rawref ref=").expect("marker must be present");
    assert!(marker_pos > 0);
    assert_eq!(
        out.stdout[marker_pos - 1],
        b'\n',
        "marker must be on its own line"
    );

    // Progress `... ok` lines must be dropped
    assert!(!s.contains("... ok"), "ok progress lines must be stripped");
    // Running header and summary must be preserved
    assert!(
        s.contains("running 105 tests"),
        "running header must be preserved"
    );
    assert!(
        s.contains("test result: ok. 105 passed"),
        "summary must be preserved"
    );

    // Single execution
    assert_eq!(read_exec_count(&counter_path), 1, "single execution");

    // Byte-exact recovery
    let ref_id = extract_ref_id(&s).expect("must have ref_id");
    let recovered = rawref_cmd(&tmp, bin.path())
        .args(["output", "get", &ref_id])
        .output()
        .unwrap();
    assert!(recovered.status.success());
    assert_eq!(recovered.stdout, content.as_bytes(), "byte-exact recovery");

    // `output get` must NOT re-run cargo
    assert_eq!(
        read_exec_count(&counter_path),
        1,
        "output get must not re-execute cargo"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// p04 — never-worse: applied display (including marker) strictly < raw bytes
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn p04_never_worse_display_strictly_smaller_than_raw() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();

    let content = pytest_passing_fixture(110);
    let fixture_path = tmp.path().join("fixture.txt");
    fs::write(&fixture_path, &content).unwrap();
    let counter_path = tmp.path().join("counter");
    write_pytest_script(&bin.path().join("pytest"), &fixture_path, &counter_path, 0);

    let out = rawref_cmd(&tmp, bin.path()).arg("pytest").output().unwrap();

    // The full stdout (condensed content + marker) must be strictly < raw bytes
    let display_len = out.stdout.len();
    let raw_len = content.len();

    // Verify specialised reducer was applied (otherwise test is vacuously true)
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("view=pytest"),
        "specialized reducer must be applied"
    );

    assert!(
        display_len < raw_len,
        "never-worse violated: display={display_len} bytes must be strictly < raw={raw_len} bytes"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// p05 — malformed/parsefail → generic fallback, no view=
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn p05_parsefail_falls_back_to_generic_no_view() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();

    // Malformed pytest output: >100 lines but NO final `=== N passed ===` summary.
    // pytest reducer must return ParseFailed → generic fallback.
    let mut malformed = String::new();
    malformed.push_str(
        "============================= test session starts ==============================\n",
    );
    malformed.push_str("platform linux -- Python 3.11.0, pytest-7.4.0, pluggy-1.3.0\n");
    malformed.push_str("rootdir: /home/user/project\n");
    malformed.push_str("collected 108 items\n\n");
    for i in 1..=108usize {
        malformed.push_str(&format!(
            "tests/test_unit.py::test_fn_{i:03} PASSED    [{i:3}%]\n"
        ));
    }
    // No final summary line — causes ParseFailed in pytest reducer

    let fixture_path = tmp.path().join("malformed.txt");
    fs::write(&fixture_path, &malformed).unwrap();
    let counter_path = tmp.path().join("counter");
    write_pytest_script(&bin.path().join("pytest"), &fixture_path, &counter_path, 0);

    let out = rawref_cmd(&tmp, bin.path()).arg("pytest").output().unwrap();

    let s = String::from_utf8_lossy(&out.stdout);

    // Must NOT produce a specialized view
    assert!(
        !s.contains("view="),
        "parsefail must NOT produce view= field"
    );

    // Must fall through to generic condenser → generic marker with omitted=
    if s.contains("[rawref ref=") {
        assert!(
            s.contains("omitted="),
            "generic fallback marker must contain omitted="
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// p06 — non-UTF-8 stdout → NonUtf8 skip → generic/raw; output get byte-exact
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn p06_non_utf8_generic_output_get_byte_exact() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();

    // Build binary content: 50 ASCII lines + non-UTF-8 bytes + 65 ASCII lines = 116 lines
    let mut binary_content: Vec<u8> = Vec::new();
    for i in 0u32..50 {
        binary_content.extend_from_slice(format!("ascii line {i:03}\n").as_bytes());
    }
    // Invalid UTF-8 sequence embedded as a single "line"
    binary_content.extend_from_slice(b"\xff\xfe\x80\n");
    for i in 0u32..65 {
        binary_content.extend_from_slice(format!("more line {i:03}\n").as_bytes());
    }
    // Total: 50 + 1 + 65 = 116 lines — exceeds 100-line threshold

    let fixture_path = tmp.path().join("non_utf8.bin");
    fs::write(&fixture_path, &binary_content).unwrap();
    fs::set_permissions(&fixture_path, fs::Permissions::from_mode(0o600)).unwrap();

    // Script cats the binary fixture (counter not critical for this test)
    let script = format!(
        "#!/bin/sh\ncat {fixture}\n",
        fixture = fixture_path.display()
    );
    // Name the script "pytest" so rawref's argv normalizer matches it
    write_script(&bin.path().join("pytest"), &script);

    let out = rawref_cmd(&tmp, bin.path()).arg("pytest").output().unwrap();

    let s_lossy = String::from_utf8_lossy(&out.stdout);

    // Non-UTF-8 triggers NonUtf8 skip → no specialized pytest view
    assert!(
        !s_lossy.contains("view=pytest"),
        "non-UTF-8 must not produce pytest specialized view"
    );

    // If a ref was captured (generic condensed), verify byte-exact recovery
    if let Some(ref_id) = extract_ref_id(&s_lossy) {
        let recovered = rawref_cmd(&tmp, bin.path())
            .args(["output", "get", &ref_id])
            .output()
            .unwrap();
        assert!(
            recovered.status.success(),
            "output get must succeed for non-UTF-8 ref"
        );
        assert_eq!(
            recovered.stdout, binary_content,
            "output get must be byte-exact for non-UTF-8 content"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// p07 — short output (below threshold) → raw passthrough, no marker
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn p07_short_output_passthrough_no_marker() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();

    // 3-line pytest output — well below the 100-line / 10KB threshold
    let content = "============================= test session starts ==============================\n\
                   \n\
                   ============================== 1 passed in 0.01s ==============================\n";
    let fixture_path = tmp.path().join("short.txt");
    fs::write(&fixture_path, content).unwrap();
    let counter_path = tmp.path().join("counter");
    write_pytest_script(&bin.path().join("pytest"), &fixture_path, &counter_path, 0);

    let out = rawref_cmd(&tmp, bin.path()).arg("pytest").output().unwrap();

    let s = String::from_utf8_lossy(&out.stdout);

    // Short output → raw passthrough (no marker injected)
    assert!(
        !s.contains("[rawref ref="),
        "short output must not produce a marker"
    );
    // Original content must appear verbatim
    assert!(
        s.contains("1 passed in 0.01s"),
        "original content must pass through"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// p08 — --collect-only gate → machine-readable → generic, no view=pytest
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn p08_collect_only_gate_generic_not_specialized() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();

    let content = pytest_passing_fixture(110);
    let fixture_path = tmp.path().join("fixture.txt");
    fs::write(&fixture_path, &content).unwrap();
    let counter_path = tmp.path().join("counter");
    write_pytest_script(&bin.path().join("pytest"), &fixture_path, &counter_path, 0);

    // rawref normalizes: pytest + ["--collect-only"] → Pytest; reducer detects gate
    let out = rawref_cmd(&tmp, bin.path())
        .args(["pytest", "--collect-only"])
        .output()
        .unwrap();

    let s = String::from_utf8_lossy(&out.stdout);

    // MachineReadable gate must prevent specialized view
    assert!(
        !s.contains("view=pytest"),
        "--collect-only must skip specialized reducer (no view=pytest)"
    );
    // Generic fallback: marker present with omitted=
    assert!(
        s.contains("[rawref ref=") && s.contains("omitted="),
        "--collect-only must produce generic marker with omitted="
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// p08b — --message-format=json gate → machine-readable → generic, no view=cargo-test
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn p08b_message_format_json_gate_generic_not_specialized() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();

    let content = cargo_passing_fixture(105);
    let fixture_path = tmp.path().join("fixture.txt");
    fs::write(&fixture_path, &content).unwrap();
    let counter_path = tmp.path().join("counter");
    write_cargo_script(&bin.path().join("cargo"), &fixture_path, &counter_path, 0);

    // rawref: cargo + ["test", "--message-format=json"] → CargoTest; reducer detects gate
    let out = rawref_cmd(&tmp, bin.path())
        .args(["cargo", "test", "--message-format=json"])
        .output()
        .unwrap();

    let s = String::from_utf8_lossy(&out.stdout);

    assert!(
        !s.contains("view=cargo-test"),
        "--message-format=json must skip specialized reducer (no view=cargo-test)"
    );
    assert!(
        s.contains("[rawref ref=") && s.contains("omitted="),
        "--message-format=json must produce generic marker with omitted="
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// p09 — RAWREF_REDUCERS=0 → generic behaviour, no view=
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn p09_reducers_disabled_generic_no_view() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();

    let content = pytest_passing_fixture(110);
    let fixture_path = tmp.path().join("fixture.txt");
    fs::write(&fixture_path, &content).unwrap();
    let counter_path = tmp.path().join("counter");
    write_pytest_script(&bin.path().join("pytest"), &fixture_path, &counter_path, 0);

    let out = rawref_cmd(&tmp, bin.path())
        .arg("pytest")
        .env("RAWREF_REDUCERS", "0")
        .output()
        .unwrap();

    let s = String::from_utf8_lossy(&out.stdout);

    // No specialized view with reducers disabled
    assert!(
        !s.contains("view="),
        "RAWREF_REDUCERS=0 must not produce view= field"
    );
    // Generic condenser must still run
    assert!(
        s.contains("[rawref ref=") && s.contains("omitted="),
        "RAWREF_REDUCERS=0 must still produce generic marker with omitted="
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// p10 — unmatched command → Generic normalisation → same as Phase 1
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn p10_unmatched_command_generic_equivalent() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();

    // `git diff` is unmatched → NormalizedCommand::Generic
    // Use `sh -c "seq 1 200"` as a deterministic unmatched command
    let out = rawref_cmd(&tmp, bin.path())
        .args(["sh", "-c", "seq 1 200"])
        .output()
        .unwrap();

    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);

    // Unmatched: must use generic pipeline only
    assert!(s.contains("[rawref ref="), "must have rawref marker");
    assert!(
        !s.contains("view="),
        "unmatched command must not have view="
    );
    assert!(
        s.contains("omitted="),
        "unmatched command must have generic omitted="
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// p11 — stash fail-open: no marker, raw output passthrough, child exit preserved
//
// Technique: set RAWREF_DATA_DIR to a regular FILE (not a directory).
// `Stash::open` calls `fs::create_dir_all(data_dir)` which fails with EEXIST
// when the path already exists as a file, reliably triggering fail-open.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn p11_stash_failopen_no_marker_raw_child_exit() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();

    // Create fixture and script in bin dir
    let content = pytest_passing_fixture(110);
    let fixture_path = bin.path().join("p11_fixture.txt");
    fs::write(&fixture_path, &content).unwrap();
    let counter_path = bin.path().join("p11_counter");
    write_pytest_script(&bin.path().join("pytest"), &fixture_path, &counter_path, 0);

    // Create a REGULAR FILE at the data-dir path to make Stash::open fail.
    // fs::create_dir_all(file_path) returns EEXIST when the path is a file,
    // so the stash is never opened and rawref goes fail-open.
    let fake_data_dir = tmp.path().join("not_a_directory.txt");
    fs::write(&fake_data_dir, b"I am a file, not a directory").unwrap();

    let orig_path = std::env::var("PATH").unwrap_or_default();
    let out = Command::cargo_bin("rawref")
        .unwrap()
        .env("RAWREF_DATA_DIR", &fake_data_dir)
        .env("PATH", format!("{}:{}", bin.path().display(), orig_path))
        .arg("pytest")
        .output()
        .unwrap();

    // Child exit 0 must be preserved (fail-open never alters exit code)
    assert!(
        out.status.success(),
        "fail-open must preserve child exit code 0"
    );

    // Stash open failed → raw passthrough → no rawref ref marker
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        !s.contains("[rawref ref="),
        "stash fail-open must not produce a rawref marker; stdout was:\n{s}"
    );

    // Raw content must be present (fail-open writes raw child stdout verbatim)
    assert!(
        s.contains("110 passed in 2.34s"),
        "raw child stdout must appear in fail-open output"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// p12 — pytest failure exit code passthrough; marker still produced
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn p12_pytest_failure_exit_passthrough() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();

    let content = pytest_failing_fixture(103);
    let fixture_path = tmp.path().join("fixture_fail.txt");
    fs::write(&fixture_path, &content).unwrap();
    let counter_path = tmp.path().join("counter");
    // Exit code 1 — standard pytest failure exit
    write_pytest_script(&bin.path().join("pytest"), &fixture_path, &counter_path, 1);

    let out = rawref_cmd(&tmp, bin.path()).arg("pytest").output().unwrap();

    // Exit code 1 from pytest must be preserved
    assert_eq!(
        out.status.code(),
        Some(1),
        "pytest failure exit code 1 must be preserved"
    );

    let s = String::from_utf8_lossy(&out.stdout);
    // A rawref marker must still be present (stash succeeded)
    assert!(
        s.contains("[rawref ref="),
        "must have rawref marker even on failure"
    );
    // Specialized view expected (output is well-formed with final summary)
    assert!(
        s.contains("view=pytest"),
        "failing pytest must still produce view=pytest"
    );
    // Failure block must be preserved in the reduced output
    assert!(
        s.contains("FAILED"),
        "FAILED text must be preserved for failing pytest"
    );

    assert_eq!(read_exec_count(&counter_path), 1, "single execution");
}

// ─────────────────────────────────────────────────────────────────────────────
// p12b — cargo test failure exit code passthrough; view=cargo-test present
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn p12b_cargo_failure_exit_view_present() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();

    let content = cargo_failing_fixture(103);
    let fixture_path = tmp.path().join("fixture_fail.txt");
    fs::write(&fixture_path, &content).unwrap();
    let counter_path = tmp.path().join("counter");
    // Cargo test exits 101 on failure
    write_cargo_script(&bin.path().join("cargo"), &fixture_path, &counter_path, 101);

    let out = rawref_cmd(&tmp, bin.path())
        .args(["cargo", "test"])
        .output()
        .unwrap();

    assert_eq!(
        out.status.code(),
        Some(101),
        "cargo test failure exit code 101 must be preserved"
    );

    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        s.contains("[rawref ref="),
        "must have rawref marker on cargo failure"
    );
    assert!(
        s.contains("view=cargo-test"),
        "must have view=cargo-test on failure"
    );
    // Failure details must be preserved
    assert!(
        s.contains("tests::test_the_failure"),
        "failed test name must be in reduced output"
    );
    assert!(
        s.contains("test result: FAILED"),
        "FAILED summary must be preserved"
    );

    assert_eq!(read_exec_count(&counter_path), 1, "single execution");
}

// ─────────────────────────────────────────────────────────────────────────────
// p13 — marker contract:
//   • extract_ref_id works on both generic and specialized markers
//   • generic: contains omitted=, no view=
//   • specialized: contains recoverability=retrievable, view=, no omitted=
//   • specialized marker is on its own line (byte before is '\n')
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn p13_marker_contract_generic_vs_specialized() {
    // ── generic marker ────────────────────────────────────────────────────────
    let tmp_gen = TempDir::new().unwrap();
    let bin_gen = TempDir::new().unwrap();

    let gen_out = rawref_cmd(&tmp_gen, bin_gen.path())
        .args(["sh", "-c", "seq 1 200"])
        .output()
        .unwrap();
    let gen_s = String::from_utf8_lossy(&gen_out.stdout);

    assert!(gen_s.contains("[rawref ref="), "generic must have marker");
    let gen_ref = extract_ref_id(&gen_s).expect("extract_ref_id must succeed on generic marker");
    assert_eq!(gen_ref.len(), 32, "generic ref must be 32 hex chars");
    assert!(
        gen_s.contains("omitted="),
        "generic marker must have omitted="
    );
    assert!(
        !gen_s.contains("view="),
        "generic marker must NOT have view="
    );

    // ── specialized marker (pytest) ───────────────────────────────────────────
    let tmp_spec = TempDir::new().unwrap();
    let bin_spec = TempDir::new().unwrap();

    let content = pytest_passing_fixture(110);
    let fixture_path = tmp_spec.path().join("fixture.txt");
    fs::write(&fixture_path, &content).unwrap();
    let counter_path = tmp_spec.path().join("counter");
    write_pytest_script(
        &bin_spec.path().join("pytest"),
        &fixture_path,
        &counter_path,
        0,
    );

    let spec_out = rawref_cmd(&tmp_spec, bin_spec.path())
        .arg("pytest")
        .output()
        .unwrap();
    let spec_s = String::from_utf8_lossy(&spec_out.stdout);

    assert!(
        spec_s.contains("[rawref ref="),
        "specialized must have marker"
    );
    let spec_ref =
        extract_ref_id(&spec_s).expect("extract_ref_id must succeed on specialized marker");
    assert_eq!(spec_ref.len(), 32, "specialized ref must be 32 hex chars");
    assert!(
        !spec_s.contains("omitted="),
        "specialized marker must NOT have omitted="
    );
    assert!(
        spec_s.contains("view=pytest"),
        "specialized must have view=pytest"
    );
    assert!(
        spec_s.contains("recoverability=retrievable"),
        "specialized must have recoverability=retrievable"
    );

    // Marker must occupy its own line
    let pos = spec_s.find("[rawref ref=").expect("marker must be present");
    assert!(pos > 0);
    assert_eq!(
        spec_out.stdout[pos - 1],
        b'\n',
        "specialized marker must be on its own line"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// p14 — cargo stderr compile errors → NOT view=cargo-test on stderr
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn p14_cargo_stderr_compile_errors_no_specialized_view() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();

    // Build a >100-line compile-error stderr fixture
    let mut compile_errors = String::new();
    compile_errors.push_str("   Compiling mylib v0.1.0 (/home/user/mylib)\n");
    for i in 1u32..=52 {
        compile_errors.push_str(&format!(
            "error[E0308]: mismatched types\n  --> src/lib.rs:{}:{}: expected `i32`, found `&str`\n",
            i * 2,
            i
        ));
    }
    compile_errors.push_str("error: aborting due to 52 previous errors\n");
    compile_errors
        .push_str("For more information about this error, try `rustc --explain E0308`.\n");
    compile_errors.push_str("error: could not compile `mylib` due to 52 previous errors\n");
    // Total: 1 + 52*2 + 3 = 108 lines → exceeds 100-line threshold on stderr

    let stderr_fixture_path = tmp.path().join("compile_errors.txt");
    fs::write(&stderr_fixture_path, &compile_errors).unwrap();

    // cargo script: empty stdout, compile errors on stderr, exit 101
    let script = format!(
        concat!(
            "#!/bin/sh\n",
            "if [ \"$1\" != \"test\" ]; then exit 1; fi\n",
            "cat {stderr_fixture} >&2\n",
            "exit 101\n",
        ),
        stderr_fixture = stderr_fixture_path.display(),
    );
    write_script(&bin.path().join("cargo"), &script);

    let out = rawref_cmd(&tmp, bin.path())
        .args(["cargo", "test"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(101));

    // stdout: empty → raw passthrough, no marker
    let stdout_s = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout_s.contains("view=cargo-test"),
        "empty stdout must not have view=cargo-test"
    );

    // stderr: compile diagnostics → cargo reducer skips (stderr channel) → generic/raw
    let stderr_s = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr_s.contains("view=cargo-test"),
        "compile error stderr must not have view=cargo-test"
    );
    // stderr note: rawref may emit its own stash-related messages on stderr too;
    // the critical invariant is absence of view=cargo-test
}

// ─────────────────────────────────────────────────────────────────────────────
// p15 — successful pytest → no FAILED text in display
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn p15_success_pytest_no_failed_text() {
    let tmp = TempDir::new().unwrap();
    let bin = TempDir::new().unwrap();

    let content = pytest_passing_fixture(110);
    let fixture_path = tmp.path().join("fixture.txt");
    fs::write(&fixture_path, &content).unwrap();
    let counter_path = tmp.path().join("counter");
    write_pytest_script(&bin.path().join("pytest"), &fixture_path, &counter_path, 0);

    let out = rawref_cmd(&tmp, bin.path()).arg("pytest").output().unwrap();

    let s = String::from_utf8_lossy(&out.stdout);

    // Success path: reducer must never inject FAILED text
    assert!(
        !s.contains("FAILED"),
        "successful pytest must not contain FAILED text in display; got:\n{s}"
    );
    // Sanity: verify it was actually applied (otherwise test is vacuously true)
    assert!(
        s.contains("view=pytest"),
        "must be applied by pytest reducer"
    );
}
