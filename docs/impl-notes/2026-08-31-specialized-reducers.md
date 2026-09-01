# Phase 2 — Specialized Reducers Implementation Notes

> **Date**: 2026-08-31
> **Scope**: Phase 2 implementation (Waves 0–5: foundation, argv, pytest, cargo test, integration)
> **Status**: ✅ Complete; final review and independent real-tool verification passed
> **Baseline**: Phase 1 commit `b1b72dc` (92/92 tests) → Phase 2 (303/303 tests, +211 new)

---

## 1. Implementation Scope

This document records the **complete Phase 2 implementation** of specialized reducers for pytest and cargo test, including the display pipeline architecture and all implementation details.

### Foundation (Waves 0–2, complete)
- `src/argv.rs` — NormalizedCommand enum, normalize() function
- `src/display/{context, outcome, marker, registry, mod}.rs` — pipeline infrastructure
- `src/display/generic.rs` — Phase 1 generic head/tail algorithm
- `src/main.rs` — updated to call display::render_passthrough with FOLDBACK_REDUCERS env

### Reducer Implementations (Waves 3–4, complete)
- `src/display/reducers/pytest.rs` — complete implementation with **39 source-level unit tests**
- `tests/reducers_pytest.rs` — **68 focused reducer/pipeline tests**
- `src/display/reducers/cargo_test.rs` — complete implementation with **42 source-level unit tests**

### Integration & E2E (Wave 5, complete)
- `tests/integration_phase2.rs` — **18 black-box tests** using simulated pytest/cargo scripts via TempDir
- `tests/fixtures/pytest/*.txt` — 9 pytest fixtures, including modern quiet-mode pass/fail output
- `tests/fixtures/cargo-test/*.txt` — 7 cargo test fixtures, including mixed multi-binary pass/fail output

**Total tests**: 303/303 passing (92 Phase 1 preserved + 211 Phase 2 new).

---

## 2. Display Pipeline Architecture

### Three-level fallback

```
raw stdout/stderr
    ├─1. Specialized reducer (pytest / cargo-test)
    │   └─ if matches & enabled & not gated & parse succeeds & beneficial
    │       → ReduceOutcome with view=pytest|cargo-test, applied=true
    ├─2. Generic head/tail (Phase 1 algorithm)
    │   └─ if specialized skipped/failed/not-beneficial
    │       → ReduceOutcome with view=Generic, applied=true
    └─3. Raw passthrough
        └─ if generic also not beneficial
            → ReduceOutcome with applied=false
```

All three levels check `beneficial()`: `candidate.len() < raw.len()` (strict `<`, marker bytes included).

### Never-worse guarantee

Every display view — whether specialized, generic, or raw — is byte-smaller than the original when applied, or not applied at all. **Marker bytes are counted in the never-worse check.**

---

## 3. pytest Reducer (39 source unit tests + 68 focused tests)

**Location**: `src/display/reducers/pytest.rs`

### Pure function interface
```rust
fn reduce(&self, input: &[u8], ctx: &ChannelContext) -> ReduceOutcome
```
No IO, no spawn, no exit mutation. Returns candidate bytes (without marker) or skip reason.

### Passthrough gates (skip before parsing)
- `--collect-only` / `--co` → `MachineReadable`
- `-v` / `--verbose` (single count) → `MachineReadable`
- `-vv` / `-vvv` (count >= 2) → `MachineReadable`
- `--json-report`, `--junitxml`, other machine-readable flags → `MachineReadable`
- Non-UTF-8 stdout → `NonUtf8`
- Empty stdout → `Empty`
- Channel::Stderr → `ParseFailed` (stderr always generic/raw)
- No final summary line → `ParseFailed`

### Preservation strategy
1. **Failure blocks**: `FAILED` lines, traceback sections, entire `FAILURES =` block — kept verbatim
2. **Error blocks**: `ERROR` lines, `ERRORS =` block — kept verbatim
3. **Short test summary**: `= short test summary info =` section — kept verbatim
4. **Warnings summary**: Capped at 5 content lines + 1 footer line with count (e.g., `… 42 warnings omitted`)
   - Skips `-- Docs:` footer when counting
5. **Final summary line**: `=N passed/failed/error/skipped/warnings in X.XXs=` — kept verbatim
6. **Noise removal**: Progress dots (`.`), `PASSED` lines, preamble headers — discarded

Modern pytest quiet-mode summaries without `=` borders are also accepted using a
strict grammar such as `9000 passed in 5.43s` or
`1 failed, 8999 passed in 5.43s`. Status words are allowlisted, durations must
contain a digit and end in `s`, and the original summary line is preserved
verbatim.

### Key fixes applied
- `-v` / `-vv` / `-vvv` detection: matches any consecutive `v`s after `-` (not just combined flags)
- Warnings footer parsing: excludes `-- Docs:` line from content count; footer itself is 1 line
- Candidate body newline: appended if absent (ensures marker on own line)

### Fixture coverage
| File | Purpose |
|------|---------|
| `passing_many.txt` | Long `.` progress output + final `passed` summary |
| `failing_one.txt` | Single failure with traceback + short summary + final summary |
| `error_setup.txt` | ERROR block (distinct from FAILURE) |
| `warnings_heavy.txt` | Many warnings, capped at 5+count format |
| `no_tests_exit5.txt` | Exit code 5 (no tests collected), edge case |
| `collect_only.txt` | Gate trigger (machine-readable) |
| `malformed.txt` | No final summary line → ParseFailed → generic fallback |
| `quiet_passing_many.txt` | Modern pytest `-q` bare passing summary |
| `quiet_failing_one.txt` | Modern pytest `-q` bare mixed failed/passed summary |

---

## 4. cargo test Reducer (42 source-level unit tests)

**Location**: `src/display/reducers/cargo_test.rs`

### Pure function interface
```rust
fn reduce(&self, input: &[u8], ctx: &ChannelContext) -> ReduceOutcome
```
No IO, no spawn, no exit mutation.

### Passthrough gates
- `--message-format` or `--message-format=*` in args → `MachineReadable`
- Non-UTF-8 stdout → `NonUtf8`
- Empty stdout → `Empty`
- Channel::Stderr → `ParseFailed` (compile errors stay generic/raw)
- No `test result:` final line → `ParseFailed`

### Preservation strategy
1. **Test result lines**: `test <name> ... ok` / `FAILED` / `ignored` — **failure-only lines kept; passes dropped**
2. **Failure blocks**: `---- <name> stdout ----` / `stderr ----` blocks — kept verbatim
3. **Failures section**: `failures:` header through `test result:` — kept with all names listed
4. **Final summary**: `test result: ok|FAILED` line with count — kept verbatim
5. **Header lines**: `running N tests` — kept

### Key design
- **Never mutates failure semantics**: If parse is incomplete or ambiguous, returns `ParseFailed` to fall through to generic/raw rather than risk false negatives
- **stderr always generic**: Compile errors (rustc diagnostics) are structurally complex; conservative choice is generic/raw, never specialized
- **Mixed results**: Every original `test result:` line is preserved verbatim. A
  mixed multi-binary fixture proves that one successful binary cannot hide a
  later failed binary; the reducer does not synthesize or reinterpret summaries.

### Fixture coverage
| File | Purpose |
|------|---------|
| `passing_many.txt` | Long `ok` progress lines + final `test result: ok` summary |
| `failing_one.txt` | Single failure with `---- <name> stdout ----` block + `failures:` section |
| `ignored_filtered.txt` | Ignored tests, count in summary |
| `multiple_binaries.txt` | Multiple binaries' test output combined |
| `mixed_multi_binary.txt` | One successful binary followed by one failed binary; anti-fabrication coverage |
| `message_format_json.txt` | Gate trigger (machine-readable) |
| `malformed.txt` | No `test result:` line → ParseFailed → generic |

---

## 5. Integration Tests (18 E2E black-box)

**Location**: `tests/integration_phase2.rs`

### Architecture
Each test:
1. Creates isolated `TempDir` for `FOLDBACK_DATA_DIR`
2. Creates isolated `TempDir` for fake-bin `PATH` 
3. Writes shell scripts (pytest, cargo, python3) that output >100-line real-style content
4. Spawns `foldback` with isolated environment
5. Verifies marker format and never-worse property

**No external pytest/cargo required**: Fake scripts output hardcoded fixture-like content, deterministic and reproducible.

### Test scenarios (E2E)
The 18 tests implement the plan's p01–p15 matrix: generic compatibility,
pytest/cargo specialized views, `python -m pytest` routing, strict never-worse,
parse and non-UTF-8 fallback, short-output passthrough, machine-readable gates,
`FOLDBACK_REDUCERS=0`, unmatched commands, stash fail-open, exit-code passthrough,
marker extraction, conservative cargo stderr handling, byte-exact retrieval, and
single-execution counters. The source file is authoritative for individual test
function names.

---

## 6. Key Decisions & Constraints

### Marker contracts
**Generic** (Phase 1 compat):
```
[foldback ref=<32hex> raw=<bytes>b lines=<n> omitted=<m> expires=<ISO8601Z>]
```
- `omitted=` is precise (head+tail line count between them)

**Specialized** (pytest/cargo):
```
[foldback ref=<32hex> raw=<bytes>b lines=<n> view=pytest mode=summary recoverability=retrievable expires=<ISO8601Z>]
```
- **No `omitted=`** (semantic recomposition, lines unmappable)
- **Contains `view=` / `mode=` / `recoverability=retrievable`**

### Environment variables
- `FOLDBACK_REDUCERS=0` disables specialized; uses only generic (Phase 1 compatible)
- Any other value (unset, `""`, `"1"`, etc.) enables specialized reducers

### argv normalization
- `pytest` or `python* -m pytest` → `NormalizedCommand::Pytest`
- `cargo test [...]` → `NormalizedCommand::CargoTest`
- Others → `NormalizedCommand::Generic`

### Reducer invariants
1. **Byte-exact inputs**: Reducers preserve raw on `output get`, no re-execution
2. **Exit code passthrough**: Reducers never modify exit code
3. **Pure functions**: No IO, no spawn, no state mutation beyond ReduceOutcome
4. **Parse conservatism**: On ambiguity, skip (parse fail) rather than misinterpret

---

## 7. Boundary Cases & Known Limits

### pytest boundary
- **exit 5 (no tests)**: Parser handles gracefully; marker applied if beneficial
- **Very many warnings**: Capped at 5 lines + 1 footer; omitted count in footer
- **Non-UTF-8**: Falls to generic (text parser can't handle)
- **Verbose flags combined**: `-vv`, `-vvv` detected; any count >= 2 gates
- **Docs footer**: `-- Docs:` line skipped from warning line count

### cargo boundary
- **Compile errors in stderr**: Never specialized; always generic/raw
- **Multiple binaries**: Parser handles multiple `test result:` lines; keeps all failure blocks
- **Ignored tests**: Counted in summary; preserved if in `failures:` section
- **nextest output**: Not recognized (falls to generic); `--message-format` catches json-like formats

### Both
- **Failure-to-success mutation**: Strictly forbidden; if uncertain, parse fail + generic
- **Binary content**: Falls to generic/raw (NonUtf8 skip reason)

---

## 8. Verified Deviations from plan

### All Q1–Q4 closed (no open questions remain)
- Q1: pytest warnings max 5 lines ✅ (implemented)
- Q2: cargo context 0 lines (block-only) ✅ (implemented)
- Q3: python* basename matching ✅ (implemented; matches `python`, `python3`, `python3.12`, etc.)
- Q4: cargo test subcommand form ✅ (implemented; argv[0]=`cargo`, argv[1]=`test`)

### CLI edge cases
- `-v` gates to generic (matches implementation); examples corrected
- `-vv`/`--verbose` (count>=2) gates; single `-v` also gates
- `--collect-only` gates (matches implementation)
- `--message-format*` gates for cargo (any format flag) ✅

---

## 9. Test Verification

### Focused checks
```bash
# Source-level unit tests
cargo test --locked --lib display::reducers::pytest      # 39 tests
cargo test --locked --lib display::reducers::cargo_test  # 42 tests

# Pytest reducer and pipeline focused tests
cargo test --locked --test reducers_pytest               # 68 tests

# Integration tests
cargo test --locked --test integration_phase2             # 18 tests

# Total
cargo test --locked                                      # 303/303 all green
```

### Quality gates
```bash
cargo fmt --check                                 # ✅ Pass
cargo clippy --all-targets -- -D warnings        # ✅ Pass
cargo build --release                            # ✅ Pass
```

---

## 10. Fixtures are Real (Glob-verified)

pytest fixtures (9):
- `tests/fixtures/pytest/malformed.txt`
- `tests/fixtures/pytest/collect_only.txt`
- `tests/fixtures/pytest/no_tests_exit5.txt`
- `tests/fixtures/pytest/warnings_heavy.txt`
- `tests/fixtures/pytest/error_setup.txt`
- `tests/fixtures/pytest/failing_one.txt`
- `tests/fixtures/pytest/passing_many.txt`
- `tests/fixtures/pytest/quiet_passing_many.txt`
- `tests/fixtures/pytest/quiet_failing_one.txt`

cargo-test fixtures (7):
- `tests/fixtures/cargo-test/malformed.txt`
- `tests/fixtures/cargo-test/message_format_json.txt`
- `tests/fixtures/cargo-test/ignored_filtered.txt`
- `tests/fixtures/cargo-test/multiple_binaries.txt`
- `tests/fixtures/cargo-test/failing_one.txt`
- `tests/fixtures/cargo-test/passing_many.txt`
- `tests/fixtures/cargo-test/mixed_multi_binary.txt`

E2E scripts (simulated, no external dependencies):
- Fake pytest script outputs >100-line fixture-like content via `TempDir` in integration tests
- Fake cargo script outputs >100-line fixture-like content via `TempDir` in integration tests
- Both deterministic and reproducible; no pytest/cargo installation required

---

## 11. Remaining Work

**Phase 2 Implementation**: ✅ Complete (both reducers + integration)

**Phase 2 final review and independent verification**: ✅ Passed. Real temporary
Cargo projects and pytest 8.4.2 pass/fail runs produced specialized markers,
preserved exit codes and failure evidence, and recovered raw bytes with matching
size and SHA-256.

**Phase 3** (not in scope):
- git diff / git status reducers
- Streaming spill-to-disk (large output)
- Stats / quota / metrics (token savings, retrieve hit rate)
- SQLite schema migration (persist reduction_kind, recoverability)
