# foldback

**Reversible CLI output capture** — run a command once, see condensed terminal output
immediately, recover the full byte-exact stream later via a short ref handle.

foldback is an **independent Rust CLI**. It references the *design ideas* behind
[RTK](https://github.com/rtk-ai/rtk) and
[ANOLISA Tokenless](https://github.com/alibaba/anolisa/tree/main/src/tokenless),
but **does not fork RTK, does not copy their source code, and is not compatible
with either project**. Storage format, CLI surface, and correctness contracts are
foldback's own.

---

## Why not just `tee` or RTK?

| | **tee** | **RTK** | **foldback** |
|---|---------|---------|------------|
| Invocation | wrap command | transparent hook / filter | explicit `foldback <cmd>` prefix |
| Terminal view | full output | often lossy compression | condensed head/tail + ref marker |
| Full output | file on disk (you manage paths) | often **not** recoverable on success | **always** stashed before condensing |
| Retrieval | `cat` the file | re-run or limited undo | `foldback output get/tail/grep <ref>` |
| Exit code | passthrough | passthrough | child exit code always passthrough |

**vs `tee`:** foldback automatically condenses long output for the terminal, assigns
an opaque ref, and stores stdout/stderr separately with metadata (command, cwd,
SHA-256, TTL). You do not pick filenames or remember paths.

**vs RTK:** RTK focuses on shrinking what the agent sees, often irreversibly on
successful runs. foldback's **raw-first** rule means complete stdout/stderr bytes
are written to local storage *before* any condensing. If storage fails, foldback
**fail-opens** — it prints the original output and still exits with the child's
code.

---

## Installation

Requires **Rust stable** and a **Unix-like OS** (macOS, Linux).

```bash
# Install to ~/.cargo/bin
cargo install --path .

# Or build and copy manually
cargo build --release
cp target/release/foldback ~/.local/bin/
```

Verify:

```bash
foldback echo hello
# hello
```

---

## Quick start (smoke test)

Isolated storage for one session:

```bash
export FOLDBACK_DATA_DIR="$(mktemp -d)"

# Long output is condensed; a ref marker is inserted
foldback sh -c "seq 1 200"
# 1 … 20
# [foldback ref=<32-hex> raw=…b lines=200 omitted=… expires=…Z]
# 181 … 200

# Copy the 32-char hex ref from the marker, then recover byte-exact stdout
REF="<paste-ref-here>"
foldback output get "$REF" | shasum -a 256
foldback output info "$REF"
```

Cleanup:

```bash
foldback output purge --expired   # removes refs past TTL
rm -rf "$FOLDBACK_DATA_DIR"
```

---

## Usage

### RTK-style direct prefix (main path)

```
foldback <command> [args...]
```

Examples:

```bash
foldback git diff
foldback pytest -q tests/
foldback cargo test --lib
foldback sh -c "seq 1 200"
```

Behavior:

1. Run the child process **once** (stdout/stderr captured via pipes, not a TTY).
2. **Save full raw** stdout and stderr to local storage (same ref for both channels).
3. Write **condensed** (or unchanged) output to your terminal.
4. Exit with the **child's exit code** (including `128 + signal` on Unix).

Short output (≤ 100 lines **and** ≤ 10 KB per channel) passes through unchanged
with **no marker**.

Long output shows head (20 lines) + marker + tail (20 lines):

```text
1
2
…
20
[foldback ref=059c83636c8ec30177c6c188d63fb55e raw=892b lines=200 omitted=160 expires=2026-09-07T08:54:42Z]
181
182
…
200
```

Marker fields: `ref` (32 lowercase hex chars), `raw` (original byte length),
`lines`, `omitted`, `expires` (UTC RFC 3339).

### Escape hatch — `foldback run --`

The first argument `output` and `run` are reserved for foldback itself. To execute
an external command literally named `output` or `run`:

```bash
foldback run -- output --help
foldback run -- run --version
```

Syntax: `foldback run -- <command> [args...]`

### Recover captured output — `foldback output …`

Recovery commands **never condense**. They write raw bytes to stdout.

| Subcommand | Syntax | Defaults |
|------------|--------|----------|
| **get** | `get <ref> [--channel stdout\|stderr\|both] [--offset N] [--limit N]` | channel=`stdout` |
| **tail** | `tail <ref> [--channel stdout\|stderr] [--lines N]` | channel=`stdout`, lines=`10` |
| **grep** | `grep <ref> <pattern> [--channel stdout\|stderr\|both]` | channel=`both` |
| **info** | `info <ref>` | — |
| **purge** | `purge --expired` | deletes expired refs + blobs; prints count |

```bash
# Full stdout (default channel)
foldback output get "$REF"

# Specific channel or byte slice
foldback output get "$REF" --channel stderr
foldback output get "$REF" --channel both
foldback output get "$REF" --offset 1024 --limit 4096

# Last N lines (stdout or stderr only)
foldback output tail "$REF" --lines 20
foldback output tail "$REF" --channel stderr --lines 5

# Literal substring match (not regex)
foldback output grep "$REF" "ERROR"
foldback output grep "$REF" "item 1" --channel stdout

# Metadata (command, cwd, sizes, SHA-256, expiry)
foldback output info "$REF"

# Housekeeping
foldback output purge --expired
# purged 3 expired ref(s)
```

`grep` uses **substring** matching per line, not regular expressions.

---

## Environment

| Variable | Default | Purpose |
|----------|---------|---------|
| `FOLDBACK_DATA_DIR` | `$XDG_DATA_HOME/foldback` or `~/.local/share/foldback` | Stash root (SQLite + blobs) |
| `XDG_DATA_HOME` | `~/.local/share` | Used when `FOLDBACK_DATA_DIR` is unset |

Set `FOLDBACK_DATA_DIR` to isolate projects, CI runs, or tests:

```bash
FOLDBACK_DATA_DIR=/tmp/my-foldback foldback echo test
```

Layout:

```text
$FOLDBACK_DATA_DIR/
├── meta.db          # SQLite (WAL)
└── blobs/
    ├── <ref_id>.stdout
    └── <ref_id>.stderr
```

---

## Guarantees

### Raw-first

For every passthrough run, foldback attempts to persist **complete** stdout and
stderr **before** condensing anything for display. Condensing reads the in-memory
capture; it does not replace what was stored.

If stash open/write fails, foldback prints the **original** stdout/stderr (fail-open),
logs a warning on stderr, and still returns the child's exit code. No ref marker
is emitted in that case.

### Byte-exact recovery

`foldback output get` returns stored bytes unchanged. Invalid UTF-8 and binary data
are preserved. `output info` records per-channel SHA-256 digests for verification.

The underlying command is **never re-run** on get/tail/grep/info.

### Exit codes

**Passthrough** (`foldback <cmd>` / `foldback run -- …`):

| Situation | Exit code |
|-----------|-----------|
| Child exits normally | child's code (0–255) |
| Child killed by signal (Unix) | `128 + signal` |
| Command not found / exec failure | `127` |
| Stash failure | **child's code** (fail-open) |

**Management** (`foldback output …`):

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Ref not found or expired |
| 2 | Bad input / invalid ref format / missing args |
| 3 | Internal storage or I/O error |

Management codes (0–3) are separate from passthrough codes. Calling `foldback` with
no arguments prints usage and exits **2**.

Ref IDs must be exactly **32 ASCII hex characters**; otherwise exit **2**.

---

## Specialized output reduction (Phase 2)

**pytest** and **cargo test** output is automatically reduced:

```bash
# Long pytest output is condensed to show failures, errors, and warnings summary
foldback pytest tests/ --tb=short
# [condensed stdout with failure blocks, short test summary, warnings]
# [foldback ref=<32-hex> raw=<bytes>b lines=<n> view=pytest mode=summary recoverability=retrievable expires=…Z]

# Note: pytest with -v/--verbose gates to generic condensing (see below)

# Long cargo test output is condensed to show failed test names and blocks
foldback cargo test --lib -- --nocapture
# [condensed stdout with failed test names, failure blocks, final summary]
# [foldback ref=<32-hex> raw=<bytes>b lines=<n> view=cargo-test mode=summary recoverability=retrievable expires=…Z]

# Unknown commands use generic head/tail condensing
foldback seq 1 500
# [head 20 lines]
# [foldback ref=<32-hex> raw=<bytes>b lines=<n> omitted=<m> expires=…Z]
# [tail 20 lines]
```

**Passthrough gates** — specialized reduction is skipped for machine-readable outputs:

```bash
# pytest: -v, -vv, --verbose, --collect-only, --json-report, --junitxml → uses generic
foldback pytest tests/ -v          # Falls back to generic head/tail (no view= in marker)

# cargo: --message-format=json, --message-format=terse → uses generic
foldback cargo test --message-format json  # Falls back to generic head/tail
```

**Opt out** of all specialized reduction (Phase 1 generic head/tail only):

```bash
FOLDBACK_REDUCERS=0 foldback pytest tests/
# [always uses Phase 1 generic condenser, no view= field in marker]
```

### Marker fields explained

**Generic (Phase 1 style)**:
- `ref` — 32-char hex ref handle for recovery
- `raw` — total original bytes
- `lines` — total original lines
- `omitted` — lines hidden between head and tail
- `expires` — UTC RFC 3339 expiry time (7 days by default)

**Specialized (pytest / cargo test)**:
- `ref`, `raw`, `lines`, `expires` — as above
- `view` — `pytest` or `cargo-test`
- `mode` — always `summary` (semantic reduction applied)
- `recoverability` — always `retrievable` (full raw stashed via `output get`)
- **Note**: specialized marker does **not** include `omitted=` (content is semantically recomposed, not truncated)

**Inline vs. end-to-end**: Specialized summary is **lossy on the terminal** (progress lines etc. removed) but **fully recoverable** via `foldback output get <ref>`.

---

## Limitations (current MVP)

These are **not** implemented yet; do not expect them to work:

- **No transparent hook** — agents must explicitly prefix commands with `foldback`
  (no Cursor/Claude command rewriting).
- **No interactive TTY** — piped capture only; editors, password prompts, and
  interactive shells will not behave correctly.
- **No watch / server mode** — bounded-lifetime commands only; no streaming
  compression or file watching.
- **No Windows** — Unix blob permissions (`0600`) and signal exit semantics.
- **No more command-specific reducers yet** — Phase 2 covers pytest and cargo test;
  `git diff`, `git status`, npm/tsc/eslint, and other commands use generic head/tail.
- **ANSI-colored test output is not parsed specially** — forced color such as
  pytest `--color=yes` may conservatively fall back to generic head/tail.
- **No stdout/stderr interleaving** — channels are stored separately; original
  cross-channel timing is lost. `--channel both` returns stdout bytes then stderr
  bytes.
- **Shell builtins** — use `foldback sh -c '…'`; foldback execs argv[0] directly.
- **Full capture in memory** — very large output loads entirely into RAM; no
  spill-to-disk yet.
- **No disk quota** — oversized command output can fill the volume.
- **Manual expiry cleanup** — `purge --expired` only; no background daemon.
- **grep is substring-only** — not regex.

---

## Privacy and disk use

foldback writes **everything** the command prints to **local disk**:

- Blob files are owner-read/write only (`0600`).
- Default **TTL is 7 days**; expired refs refuse reads (exit 1).
- Ref IDs are random 128-bit values — not derived from content.

**Threat model:** anyone with access to your user account (or a backup of
`FOLDBACK_DATA_DIR`) can read captured command output, which may include secrets,
tokens, or private paths. Use an isolated `FOLDBACK_DATA_DIR` for sensitive work,
purge expired refs, and avoid wrapping commands that emit credentials.

Stash failure is fail-open: you still see output in the terminal even when disk
writes fail.

---

## Design references

foldback's condensed-output and reversible-stash **ideas** were informed by:

- **[RTK](https://github.com/rtk-ai/rtk)** — CLI output reduction for agent
  contexts. Referenced for approach, not code.
- **[ANOLISA Tokenless](https://github.com/alibaba/anolisa/tree/main/src/tokenless)** —
  reversible stash protocol in agent frameworks. foldback exposes similar recovery
  via explicit CLI (`output get`) instead of in-framework hooks.

No source from either project was copied. foldback is MIT-licensed independently.

Further reading for maintainers:

- [`docs/plan.md`](docs/plan.md) — Phase 1 implementation plan and invariants
- [`docs/design.md`](docs/design.md) — architecture, module boundaries, and detailed design

---

## License

MIT — see [LICENSE](LICENSE).
