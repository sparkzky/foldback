# Breaking rename to foldback (2026-09-01)

**Status:** complete  
**Scope:** 0.1 pre-release, no migration provided  

---

## Summary

The product was renamed to **foldback** before the 0.1 release.
This was a pre-release rename across every public-facing contract.
No backwards compatibility aliases are provided, and no migration path exists.

---

## Exact breaking changes

### Binary name

The CLI binary is now `foldback`. Update all shell aliases, scripts, and tooling
that invoke the old binary name.

```bash
# Now
foldback echo hello
```

### Marker prefix

All condensed-output markers now use `[foldback ref=…` as the opening prefix.
The ref field format (`ref=<32hex>`), `raw=`, `lines=`, `omitted=`, `expires=`,
`view=`, `mode=`, and `recoverability=` fields are **unchanged** — only the
bracket-prefix changed.

Any scripts that parse marker lines must update their grep/sed/awk patterns
to match the new `[foldback ref=` prefix.

### Environment variables

| Old | New |
|-----|-----|
| previous `_DATA_DIR` variable | `FOLDBACK_DATA_DIR` |
| previous `_REDUCERS` variable | `FOLDBACK_REDUCERS` |

The new variable names are:
- **`FOLDBACK_DATA_DIR`** — overrides the stash storage directory.
- **`FOLDBACK_REDUCERS`** — set to `0` to disable specialized reducers.

Shell profiles, `.env` files, and CI configurations must export the new names.
The old names are silently ignored.

### Default data directory

The default stash location changed from `<XDG_DATA_HOME>/<old-name>/` to
`$XDG_DATA_HOME/foldback` (or `~/.local/share/foldback` when `XDG_DATA_HOME`
is unset). Existing stash data at the old path is NOT automatically migrated.

To preserve old refs, move the directory manually before first use.

### Rust crate API

| Old | New |
|-----|-----|
| lib crate name | `foldback_lib` |
| error type | `FoldbackError` |

Any downstream Rust code depending on the old lib crate must update `Cargo.toml`
and all `use` paths accordingly.

---

## Why no migration / compatibility aliases

This rename happened before the 0.1 release. No user-facing stable release existed
under the prior name, so there is no installed base that requires an upgrade path.
Providing aliases would add permanent maintenance cost and naming confusion without
benefiting anyone. A clean cut is the correct policy at pre-release.

---

## Repo-local ignore paths (if applicable)

If your project's `.gitignore`, `.cursorignore`, or similar files reference
local data paths from the old name, update them:

| Old pattern | New pattern |
|-------------|-------------|
| `.rawref/` | `.foldback/` |
| `rawref-data/` | `foldback-data/` |
