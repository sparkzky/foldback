use crate::error::FoldbackError;
use chrono::{DateTime, Utc};
use rand::Rng;
use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Default TTL for stored refs.
pub const DEFAULT_TTL_SECS: i64 = 7 * 24 * 3600; // 7 days

pub struct Stash {
    db: Connection,
    blobs_dir: PathBuf,
}

/// Arguments for saving a captured execution.
pub struct SaveArgs<'a> {
    pub command: &'a str,
    pub args: &'a [String],
    pub cwd: &'a str,
    pub exit_code: i32,
    pub stdout: &'a [u8],
    pub stderr: &'a [u8],
    pub ttl_secs: i64,
}

/// Metadata row as stored in SQLite.
#[derive(Debug, Clone)]
pub struct EntryMeta {
    pub ref_id: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub exit_code: i32,
    pub stdout_size: i64,
    pub stderr_size: i64,
    pub stdout_sha256: String,
    pub stderr_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Stdout,
    Stderr,
    Both,
}

impl std::str::FromStr for Channel {
    type Err = FoldbackError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "stdout" => Ok(Channel::Stdout),
            "stderr" => Ok(Channel::Stderr),
            "both" => Ok(Channel::Both),
            _ => Err(FoldbackError::BadInput(format!("unknown channel: {s}"))),
        }
    }
}

impl Stash {
    pub fn open(data_dir: &Path) -> Result<Self, FoldbackError> {
        fs::create_dir_all(data_dir)?;
        set_mode(data_dir, 0o700)?;

        let blobs_dir = data_dir.join("blobs");
        fs::create_dir_all(&blobs_dir)?;
        set_mode(&blobs_dir, 0o700)?;

        let db_path = data_dir.join("meta.db");
        let db = Connection::open(&db_path)?;
        // chmod meta.db to 0600 before enabling WAL. Sidecar files (-wal/-shm)
        // are not guaranteed to inherit these permissions; access is bounded by
        // the 0700 data_dir above. There is a narrow TOCTOU window between
        // create and chmod that we accept for the local-file threat model.
        set_mode(&db_path, 0o600)?;
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS refs (
                 ref_id       TEXT PRIMARY KEY,
                 command      TEXT NOT NULL,
                 args_json    TEXT NOT NULL,
                 cwd          TEXT NOT NULL,
                 created_at   INTEGER NOT NULL,
                 expires_at   INTEGER NOT NULL,
                 exit_code    INTEGER NOT NULL,
                 stdout_size  INTEGER NOT NULL,
                 stderr_size  INTEGER NOT NULL,
                 stdout_sha256 TEXT NOT NULL,
                 stderr_sha256 TEXT NOT NULL
             );",
        )?;

        Ok(Self { db, blobs_dir })
    }

    /// Save stdout + stderr blobs and record metadata. Returns the new ref_id.
    pub fn save(&self, a: SaveArgs<'_>) -> Result<(String, DateTime<Utc>), FoldbackError> {
        let ref_id = gen_ref_id();
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(a.ttl_secs);
        let created_ms = now.timestamp_millis();
        let expires_ms = expires_at.timestamp_millis();

        let stdout_sha = sha256_hex(a.stdout);
        let stderr_sha = sha256_hex(a.stderr);

        let stdout_path = self.blobs_dir.join(format!("{ref_id}.stdout"));
        let stderr_path = self.blobs_dir.join(format!("{ref_id}.stderr"));

        write_blob(&stdout_path, a.stdout)?;
        if let Err(e) = write_blob(&stderr_path, a.stderr) {
            // Best-effort rollback: remove stdout blob written above.
            let _ = fs::remove_file(&stdout_path);
            return Err(e);
        }

        let args_json = serde_json::to_string(a.args).unwrap_or_default();
        let db_result = self.db.execute(
            "INSERT INTO refs
             (ref_id, command, args_json, cwd, created_at, expires_at, exit_code,
              stdout_size, stderr_size, stdout_sha256, stderr_sha256)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                ref_id,
                a.command,
                args_json,
                a.cwd,
                created_ms,
                expires_ms,
                a.exit_code,
                a.stdout.len() as i64,
                a.stderr.len() as i64,
                stdout_sha,
                stderr_sha,
            ],
        );
        if let Err(e) = db_result {
            // Best-effort rollback on ordinary error paths (constraint violation,
            // I/O error, etc.).  This does NOT guarantee crash atomicity: if the
            // process is killed or panics between the blob writes and this point,
            // orphan blobs may persist.  No cross-FS+SQLite two-phase commit is
            // attempted.
            let _ = fs::remove_file(&stdout_path);
            let _ = fs::remove_file(&stderr_path);
            return Err(FoldbackError::from(e));
        }

        Ok((ref_id, expires_at))
    }

    /// Fetch metadata, checking existence and expiry.
    pub fn meta(&self, ref_id: &str) -> Result<EntryMeta, FoldbackError> {
        validate_ref_id(ref_id)?;
        let row = self.db.query_row(
            "SELECT command, args_json, cwd, created_at, expires_at, exit_code,
                    stdout_size, stderr_size, stdout_sha256, stderr_sha256
             FROM refs WHERE ref_id = ?1",
            params![ref_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i32>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, String>(8)?,
                    r.get::<_, String>(9)?,
                ))
            },
        );

        match row {
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err(FoldbackError::NotFound {
                    ref_id: ref_id.to_string(),
                });
            }
            Err(e) => return Err(FoldbackError::from(e)),
            Ok(_) => {}
        }

        let (
            command,
            args_json,
            cwd,
            created_at_ms,
            expires_at_ms,
            exit_code,
            stdout_size,
            stderr_size,
            stdout_sha256,
            stderr_sha256,
        ) = row.unwrap();

        let now_ms = Utc::now().timestamp_millis();
        if expires_at_ms < now_ms {
            return Err(FoldbackError::Expired {
                ref_id: ref_id.to_string(),
            });
        }

        let args: Vec<String> = serde_json::from_str(&args_json).unwrap_or_default();
        Ok(EntryMeta {
            ref_id: ref_id.to_string(),
            command,
            args,
            cwd,
            created_at_ms,
            expires_at_ms,
            exit_code,
            stdout_size,
            stderr_size,
            stdout_sha256,
            stderr_sha256,
        })
    }

    /// Read raw blob bytes with optional byte-offset and limit.
    /// The full blob is read and SHA-256-verified before any slice is returned.
    pub fn read_channel(
        &self,
        ref_id: &str,
        channel: Channel,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<Vec<u8>, FoldbackError> {
        let meta = self.meta(ref_id)?;
        let full = self.read_verified_full(ref_id, channel, &meta)?;
        Ok(apply_slice(full, offset, limit))
    }

    /// Return the last `n_lines` lines of the requested channel.
    /// The full blob is verified before slicing.
    pub fn tail_lines(
        &self,
        ref_id: &str,
        channel: Channel,
        n_lines: usize,
    ) -> Result<Vec<u8>, FoldbackError> {
        let meta = self.meta(ref_id)?;
        let data = self.read_verified_full(ref_id, channel, &meta)?;
        Ok(last_n_lines(&data, n_lines))
    }

    /// Return lines matching `pattern` from the requested channel.
    /// The full blob is verified before matching.
    pub fn grep_lines(
        &self,
        ref_id: &str,
        channel: Channel,
        pattern: &str,
    ) -> Result<Vec<u8>, FoldbackError> {
        let meta = self.meta(ref_id)?;
        let data = self.read_verified_full(ref_id, channel, &meta)?;
        Ok(grep_bytes(&data, pattern))
    }

    /// Remove expired refs (metadata + blobs). Returns count removed.
    pub fn purge_expired(&self) -> Result<usize, FoldbackError> {
        let now_ms = Utc::now().timestamp_millis();
        let expired: Vec<String> = {
            let mut stmt = self
                .db
                .prepare("SELECT ref_id FROM refs WHERE expires_at < ?1")?;
            let rows: Vec<String> = stmt
                .query_map(params![now_ms], |r| r.get(0))?
                .filter_map(|r| r.ok())
                .collect();
            rows
        };

        let count = expired.len();
        for ref_id in &expired {
            let _ = fs::remove_file(self.blobs_dir.join(format!("{ref_id}.stdout")));
            let _ = fs::remove_file(self.blobs_dir.join(format!("{ref_id}.stderr")));
        }
        if !expired.is_empty() {
            self.db
                .execute("DELETE FROM refs WHERE expires_at < ?1", params![now_ms])?;
        }
        Ok(count)
    }

    // ── helpers ─────────────────────────────────────────────────────────────

    fn blob_path(&self, ref_id: &str, channel: Channel) -> PathBuf {
        let ext = match channel {
            Channel::Stdout | Channel::Both => "stdout",
            Channel::Stderr => "stderr",
        };
        self.blobs_dir.join(format!("{ref_id}.{ext}"))
    }

    /// Dispatch to per-channel readers; for `Both`, stdout‖stderr are each
    /// verified independently and then concatenated before any slice.
    fn read_verified_full(
        &self,
        ref_id: &str,
        channel: Channel,
        meta: &EntryMeta,
    ) -> Result<Vec<u8>, FoldbackError> {
        match channel {
            Channel::Both => {
                let mut out = self.read_and_verify_blob(
                    ref_id,
                    Channel::Stdout,
                    meta.stdout_size,
                    &meta.stdout_sha256,
                )?;
                let stderr = self.read_and_verify_blob(
                    ref_id,
                    Channel::Stderr,
                    meta.stderr_size,
                    &meta.stderr_sha256,
                )?;
                out.extend_from_slice(&stderr);
                Ok(out)
            }
            Channel::Stdout => self.read_and_verify_blob(
                ref_id,
                Channel::Stdout,
                meta.stdout_size,
                &meta.stdout_sha256,
            ),
            Channel::Stderr => self.read_and_verify_blob(
                ref_id,
                Channel::Stderr,
                meta.stderr_size,
                &meta.stderr_sha256,
            ),
        }
    }

    /// Read the complete blob for a single channel (Stdout or Stderr), then
    /// verify byte-length and SHA-256 against stored metadata.
    ///
    /// Returns `Corrupted` — not `NotFound` or `Expired` — on any mismatch so
    /// that callers can distinguish integrity failures (exit code 3) from
    /// missing/expired refs (exit codes 1/2).
    fn read_and_verify_blob(
        &self,
        ref_id: &str,
        channel: Channel,
        expected_size: i64,
        expected_sha: &str,
    ) -> Result<Vec<u8>, FoldbackError> {
        let path = self.blob_path(ref_id, channel);
        let mut f = OpenOptions::new().read(true).open(&path)?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        if buf.len() as i64 != expected_size || sha256_hex(&buf) != expected_sha {
            return Err(FoldbackError::Corrupted {
                ref_id: ref_id.to_string(),
                channel: format!("{channel:?}"),
            });
        }
        Ok(buf)
    }
}

// ── free functions ───────────────────────────────────────────────────────────

fn gen_ref_id() -> String {
    let bytes: [u8; 16] = rand::thread_rng().gen();
    hex::encode(bytes)
}

fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    hex::encode(digest)
}

fn write_blob(path: &Path, data: &[u8]) -> Result<(), FoldbackError> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)?;
    Ok(())
}

fn validate_ref_id(ref_id: &str) -> Result<(), FoldbackError> {
    if ref_id.len() == 32 && ref_id.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(FoldbackError::InvalidRef {
            input: ref_id.to_string(),
        })
    }
}

/// Return the last `n` newline-separated lines from `data`.
pub fn last_n_lines(data: &[u8], n: usize) -> Vec<u8> {
    if data.is_empty() || n == 0 {
        return Vec::new();
    }
    let mut end = data.len();
    // Skip trailing newline when counting
    if data[end - 1] == b'\n' {
        end -= 1;
    }
    let mut count = 0;
    let mut pos = end;
    while pos > 0 {
        pos -= 1;
        if data[pos] == b'\n' {
            count += 1;
            if count == n {
                pos += 1; // include the character after the newline
                break;
            }
        }
    }
    data[pos..data.len()].to_vec()
}

fn matches_pattern(line: &[u8], pattern: &str) -> bool {
    if let Ok(s) = std::str::from_utf8(line) {
        s.contains(pattern)
    } else {
        line.windows(pattern.len()).any(|w| w == pattern.as_bytes())
    }
}

/// Apply optional byte-offset and limit to an already-verified buffer.
fn apply_slice(data: Vec<u8>, offset: Option<u64>, limit: Option<u64>) -> Vec<u8> {
    let start = offset.unwrap_or(0) as usize;
    if start >= data.len() {
        return Vec::new();
    }
    let tail = &data[start..];
    match limit {
        Some(n) => tail[..n.min(tail.len() as u64) as usize].to_vec(),
        None => tail.to_vec(),
    }
}

/// Set Unix permissions on a file or directory.  chmod errors are NOT ignored.
fn set_mode(path: &Path, mode: u32) -> Result<(), FoldbackError> {
    use std::os::unix::fs::PermissionsExt;
    let perms = fs::Permissions::from_mode(mode);
    fs::set_permissions(path, perms)?;
    Ok(())
}

fn grep_bytes(data: &[u8], pattern: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 0..data.len() {
        if data[i] == b'\n' {
            let line = &data[start..=i];
            if matches_pattern(line, pattern) {
                out.extend_from_slice(line);
            }
            start = i + 1;
        }
    }
    if start < data.len() {
        let line = &data[start..];
        if matches_pattern(line, pattern) {
            out.extend_from_slice(line);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_stash() -> (TempDir, Stash) {
        let tmp = TempDir::new().unwrap();
        let s = Stash::open(tmp.path()).unwrap();
        (tmp, s)
    }

    /// Convenience wrapper so tests stay concise.
    fn quick_save(stash: &Stash, stdout: &[u8], stderr: &[u8], ttl_secs: i64) -> String {
        stash
            .save(SaveArgs {
                command: "cmd",
                args: &[],
                cwd: "/",
                exit_code: 0,
                stdout,
                stderr,
                ttl_secs,
            })
            .unwrap()
            .0
    }

    #[test]
    fn test_save_and_meta_roundtrip() {
        let (_tmp, stash) = make_stash();
        let stdout = b"hello stdout\n";
        let stderr = b"hello stderr\n";
        let (ref_id, _) = stash
            .save(SaveArgs {
                command: "echo",
                args: &["hello".to_string()],
                cwd: "/tmp",
                exit_code: 0,
                stdout,
                stderr,
                ttl_secs: 3600,
            })
            .unwrap();
        let meta = stash.meta(&ref_id).unwrap();
        assert_eq!(meta.command, "echo");
        assert_eq!(meta.exit_code, 0);
        assert_eq!(meta.stdout_size, 13);
        assert_eq!(meta.stderr_size, 13);
        assert_eq!(meta.stdout_sha256, sha256_hex(stdout));
        assert_eq!(meta.stderr_sha256, sha256_hex(stderr));
    }

    #[test]
    fn test_byte_exact_stdout_recovery() {
        let (_tmp, stash) = make_stash();
        let stdout: Vec<u8> = (0u8..=255).collect();
        let ref_id = quick_save(&stash, &stdout, b"", 3600);
        let recovered = stash
            .read_channel(&ref_id, Channel::Stdout, None, None)
            .unwrap();
        assert_eq!(recovered, stdout);
    }

    #[test]
    fn test_byte_exact_stderr_recovery() {
        let (_tmp, stash) = make_stash();
        let stderr: Vec<u8> = vec![0u8, 255, 128, 1];
        let ref_id = quick_save(&stash, b"", &stderr, 3600);
        let recovered = stash
            .read_channel(&ref_id, Channel::Stderr, None, None)
            .unwrap();
        assert_eq!(recovered, stderr);
    }

    #[test]
    fn test_expired_ref_returns_error() {
        let (_tmp, stash) = make_stash();
        let ref_id = quick_save(&stash, b"data", b"", -1);
        let err = stash.meta(&ref_id).unwrap_err();
        assert!(matches!(err, FoldbackError::Expired { .. }));
    }

    #[test]
    fn test_not_found_ref() {
        let (_tmp, stash) = make_stash();
        let err = stash.meta("aabbccddeeff00112233445566778899").unwrap_err();
        assert!(matches!(err, FoldbackError::NotFound { .. }));
    }

    #[test]
    fn test_invalid_ref_format() {
        let (_tmp, stash) = make_stash();
        let err = stash.meta("not-a-valid-ref").unwrap_err();
        assert!(matches!(err, FoldbackError::InvalidRef { .. }));
    }

    #[test]
    fn test_offset_limit() {
        let (_tmp, stash) = make_stash();
        let ref_id = quick_save(&stash, b"0123456789", b"", 3600);
        let slice = stash
            .read_channel(&ref_id, Channel::Stdout, Some(3), Some(4))
            .unwrap();
        assert_eq!(slice, b"3456");
    }

    #[test]
    fn test_tail_lines() {
        let (_tmp, stash) = make_stash();
        let ref_id = quick_save(&stash, b"line1\nline2\nline3\nline4\nline5\n", b"", 3600);
        let tail = stash.tail_lines(&ref_id, Channel::Stdout, 2).unwrap();
        assert_eq!(tail, b"line4\nline5\n");
    }

    #[test]
    fn test_grep_lines() {
        let (_tmp, stash) = make_stash();
        let ref_id = quick_save(&stash, b"foo bar\nhello world\nfoo baz\n", b"", 3600);
        let result = stash.grep_lines(&ref_id, Channel::Stdout, "foo").unwrap();
        assert_eq!(result, b"foo bar\nfoo baz\n");
    }

    #[test]
    fn test_purge_expired() {
        let (_tmp, stash) = make_stash();
        quick_save(&stash, b"a", b"", -1);
        quick_save(&stash, b"b", b"", -1);
        quick_save(&stash, b"c", b"", 3600);
        let count = stash.purge_expired().unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_concurrent_saves_no_collision() {
        use std::thread;

        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();

        let handles: Vec<_> = (0..8)
            .map(|i| {
                let d = dir.clone();
                thread::spawn(move || {
                    let stash = Stash::open(&d).unwrap();
                    let data = format!("thread-{i}-data").into_bytes();
                    let ref_id = stash
                        .save(SaveArgs {
                            command: "cmd",
                            args: &[],
                            cwd: "/",
                            exit_code: 0,
                            stdout: &data,
                            stderr: b"",
                            ttl_secs: 3600,
                        })
                        .unwrap()
                        .0;
                    (ref_id, data)
                })
            })
            .collect();

        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let ids: std::collections::HashSet<_> = results.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids.len(), 8, "ref_id collision detected");

        let stash = Stash::open(tmp.path()).unwrap();
        for (ref_id, expected) in &results {
            let recovered = stash
                .read_channel(ref_id, Channel::Stdout, None, None)
                .unwrap();
            assert_eq!(&recovered, expected, "data mismatch for {ref_id}");
        }
    }

    #[test]
    fn test_last_n_lines() {
        assert_eq!(last_n_lines(b"a\nb\nc\n", 2), b"b\nc\n");
        assert_eq!(last_n_lines(b"a\nb\nc\n", 0), b"");
        assert_eq!(last_n_lines(b"", 5), b"");
        assert_eq!(last_n_lines(b"only\n", 1), b"only\n");
        assert_eq!(last_n_lines(b"a\nb\nc\n", 10), b"a\nb\nc\n");
    }

    #[test]
    fn test_blob_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, stash) = make_stash();
        let ref_id = quick_save(&stash, b"secret", b"sec2", 3600);
        let blob_path = stash.blobs_dir.join(format!("{ref_id}.stdout"));
        let perms = fs::metadata(&blob_path).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600, "blob not owner-only");
    }

    #[test]
    fn test_channel_both_concatenation() {
        let (_tmp, stash) = make_stash();
        let stdout = b"stdout-bytes";
        let stderr = b"stderr-bytes";
        let ref_id = quick_save(&stash, stdout, stderr, 3600);
        let both = stash
            .read_channel(&ref_id, Channel::Both, None, None)
            .unwrap();
        let mut expected = stdout.to_vec();
        expected.extend_from_slice(stderr);
        assert_eq!(both, expected);
    }

    /// `Channel::Both` with offset/limit must treat stdout‖stderr as a single
    /// logical stream and apply the range exactly once.
    /// Combined stream: "0123456789ABCDEFGHIJ" (20 bytes).
    /// offset=3, limit=4 → bytes [3..7] = "3456"  (NOT "3456DEFG").
    #[test]
    fn test_channel_both_offset_limit() {
        let (_tmp, stash) = make_stash();
        let ref_id = quick_save(&stash, b"0123456789", b"ABCDEFGHIJ", 3600);
        let slice = stash
            .read_channel(&ref_id, Channel::Both, Some(3), Some(4))
            .unwrap();
        assert_eq!(slice, b"3456");
    }

    /// Offset/limit range that straddles the stdout/stderr boundary.
    /// stdout = "AAABBB" (6 bytes), stderr = "CCCDD" (5 bytes).
    /// Combined = "AAABBBCCCDD" (11 bytes).
    /// offset=5, limit=4 → bytes [5..9] = "BCCC".
    #[test]
    fn test_channel_both_offset_limit_crosses_boundary() {
        let (_tmp, stash) = make_stash();
        let ref_id = quick_save(&stash, b"AAABBB", b"CCCDD", 3600);
        let slice = stash
            .read_channel(&ref_id, Channel::Both, Some(5), Some(4))
            .unwrap();
        assert_eq!(slice, b"BCCC");
    }

    #[test]
    fn test_offset_beyond_eof() {
        let (_tmp, stash) = make_stash();
        let ref_id = quick_save(&stash, b"hello", b"world", 3600);
        let stdout = stash
            .read_channel(&ref_id, Channel::Stdout, Some(100), None)
            .unwrap();
        assert!(
            stdout.is_empty(),
            "offset beyond stdout EOF should be empty"
        );
        let stderr = stash
            .read_channel(&ref_id, Channel::Stderr, Some(100), None)
            .unwrap();
        assert!(
            stderr.is_empty(),
            "offset beyond stderr EOF should be empty"
        );
        let both = stash
            .read_channel(&ref_id, Channel::Both, Some(100), None)
            .unwrap();
        assert!(both.is_empty(), "offset beyond both EOF should be empty");
    }

    #[test]
    fn test_limit_zero() {
        let (_tmp, stash) = make_stash();
        let ref_id = quick_save(&stash, b"0123456789", b"ABCDEFGHIJ", 3600);
        let stdout = stash
            .read_channel(&ref_id, Channel::Stdout, None, Some(0))
            .unwrap();
        assert!(stdout.is_empty(), "limit=0 on stdout should be empty");
        let both = stash
            .read_channel(&ref_id, Channel::Both, None, Some(0))
            .unwrap();
        assert!(both.is_empty(), "limit=0 on both should be empty");
    }

    #[test]
    fn test_stderr_tail_lines() {
        let (_tmp, stash) = make_stash();
        let stderr = b"err1\nerr2\nerr3\nerr4\n";
        let ref_id = quick_save(&stash, b"", stderr, 3600);
        let tail = stash.tail_lines(&ref_id, Channel::Stderr, 2).unwrap();
        assert_eq!(tail, b"err3\nerr4\n");
    }

    #[test]
    fn test_stderr_grep_lines() {
        let (_tmp, stash) = make_stash();
        let stderr = b"WARN: disk\nINFO: ok\nWARN: mem\n";
        let ref_id = quick_save(&stash, b"", stderr, 3600);
        let result = stash.grep_lines(&ref_id, Channel::Stderr, "WARN").unwrap();
        assert_eq!(result, b"WARN: disk\nWARN: mem\n");
    }

    #[test]
    fn test_binary_grep() {
        let (_tmp, stash) = make_stash();
        let mut line = vec![0xFF, 0xFE];
        line.extend_from_slice(b"XYZZY");
        line.push(b'\n');
        let mut data = line.clone();
        data.extend_from_slice(b"no-match\n");
        data.extend_from_slice(&line);
        let ref_id = quick_save(&stash, &data, b"", 3600);
        let result = stash.grep_lines(&ref_id, Channel::Stdout, "XYZZY").unwrap();
        let mut expected = line.clone();
        expected.extend_from_slice(&line);
        assert_eq!(result, expected);
    }

    #[test]
    fn test_purge_removes_blobs_and_get_unavailable() {
        let (_tmp, stash) = make_stash();
        let ref_id = quick_save(&stash, b"expired-out", b"expired-err", -1);
        let stdout_path = stash.blobs_dir.join(format!("{ref_id}.stdout"));
        let stderr_path = stash.blobs_dir.join(format!("{ref_id}.stderr"));
        assert!(stdout_path.exists());
        assert!(stderr_path.exists());

        let count = stash.purge_expired().unwrap();
        assert_eq!(count, 1);
        assert!(
            !stdout_path.exists(),
            "stdout blob should be deleted after purge"
        );
        assert!(
            !stderr_path.exists(),
            "stderr blob should be deleted after purge"
        );

        let err = stash.meta(&ref_id).unwrap_err();
        assert!(
            matches!(err, FoldbackError::NotFound { .. }),
            "purged ref should be NotFound, got {err:?}"
        );
        let read_err = stash
            .read_channel(&ref_id, Channel::Stdout, None, None)
            .unwrap_err();
        assert!(
            matches!(read_err, FoldbackError::NotFound { .. }),
            "read after purge should be NotFound, got {read_err:?}"
        );
    }

    #[test]
    fn test_purge_zero_items() {
        let (_tmp, stash) = make_stash();
        quick_save(&stash, b"alive", b"still-here", 3600);
        let count = stash.purge_expired().unwrap();
        assert_eq!(count, 0);
    }

    // ── Risk 1: orphan-blob rollback ─────────────────────────────────────────

    /// After a DB INSERT failure the two blob files that were already written
    /// must be removed.  We force the failure by injecting a BEFORE-INSERT
    /// trigger on the live connection (accessible from within this module).
    #[test]
    fn test_db_insert_failure_cleans_up_blobs() {
        let (_tmp, stash) = make_stash();
        stash
            .db
            .execute_batch(
                "CREATE TRIGGER _force_fail BEFORE INSERT ON refs \
                 BEGIN SELECT RAISE(ABORT, 'forced failure'); END;",
            )
            .unwrap();

        let result = stash.save(SaveArgs {
            command: "cmd",
            args: &[],
            cwd: "/",
            exit_code: 0,
            stdout: b"will be orphaned",
            stderr: b"also orphaned",
            ttl_secs: 3600,
        });

        assert!(result.is_err(), "save must fail when DB INSERT fails");
        let blob_files: Vec<_> = fs::read_dir(&stash.blobs_dir).unwrap().collect();
        assert_eq!(
            blob_files.len(),
            0,
            "orphan blobs should be cleaned up after DB INSERT failure"
        );
    }

    // ── Risk 2: SHA-256 integrity on every read path ─────────────────────────

    #[test]
    fn test_tampered_stdout_blob_returns_corrupted() {
        let (_tmp, stash) = make_stash();
        let ref_id = quick_save(&stash, b"original content", b"also original", 3600);
        let blob = stash.blobs_dir.join(format!("{ref_id}.stdout"));
        fs::write(&blob, b"tampered!").unwrap();

        let err = stash
            .read_channel(&ref_id, Channel::Stdout, None, None)
            .unwrap_err();
        assert!(
            matches!(err, FoldbackError::Corrupted { .. }),
            "expected Corrupted, got {err:?}"
        );
        assert_eq!(err.exit_code(), 3, "Corrupted must exit-code 3");
    }

    #[test]
    fn test_tampered_stderr_blob_returns_corrupted() {
        let (_tmp, stash) = make_stash();
        let ref_id = quick_save(&stash, b"stdout ok", b"stderr data long", 3600);
        let blob = stash.blobs_dir.join(format!("{ref_id}.stderr"));
        fs::write(&blob, b"short").unwrap(); // size mismatch
        let err = stash
            .read_channel(&ref_id, Channel::Stderr, None, None)
            .unwrap_err();
        assert!(
            matches!(err, FoldbackError::Corrupted { .. }),
            "expected Corrupted on stderr, got {err:?}"
        );
        assert_eq!(err.exit_code(), 3);
    }

    /// Range read must verify the FULL blob first, then slice — never return
    /// corrupted bytes even for a range that happens to be unmodified.
    #[test]
    fn test_tampered_blob_range_read_returns_corrupted() {
        let (_tmp, stash) = make_stash();
        let ref_id = quick_save(&stash, b"0123456789", b"", 3600);
        let blob = stash.blobs_dir.join(format!("{ref_id}.stdout"));
        fs::write(&blob, b"9876543210").unwrap(); // same size, different sha256
        let err = stash
            .read_channel(&ref_id, Channel::Stdout, Some(3), Some(4))
            .unwrap_err();
        assert!(
            matches!(err, FoldbackError::Corrupted { .. }),
            "range read on corrupted blob must return Corrupted"
        );
    }

    #[test]
    fn test_tampered_both_blob_returns_corrupted() {
        let (_tmp, stash) = make_stash();
        let ref_id = quick_save(&stash, b"stdout-ok", b"stderr-ok", 3600);
        let blob = stash.blobs_dir.join(format!("{ref_id}.stdout"));
        fs::write(&blob, b"XXXXXXXXX").unwrap();
        let err = stash
            .read_channel(&ref_id, Channel::Both, None, None)
            .unwrap_err();
        assert!(
            matches!(err, FoldbackError::Corrupted { .. }),
            "Both channel on corrupted stdout must return Corrupted"
        );
    }

    #[test]
    fn test_tampered_blob_tail_returns_corrupted() {
        let (_tmp, stash) = make_stash();
        let ref_id = quick_save(&stash, b"line1\nline2\nline3\n", b"", 3600);
        let blob = stash.blobs_dir.join(format!("{ref_id}.stdout"));
        fs::write(&blob, b"ZZZZZ\nZZZZZ\nZZZZZ\n").unwrap();
        let err = stash.tail_lines(&ref_id, Channel::Stdout, 2).unwrap_err();
        assert!(
            matches!(err, FoldbackError::Corrupted { .. }),
            "tail_lines on corrupted blob must return Corrupted"
        );
    }

    #[test]
    fn test_tampered_blob_grep_returns_corrupted() {
        let (_tmp, stash) = make_stash();
        let ref_id = quick_save(&stash, b"pattern here\nno match\n", b"", 3600);
        let blob = stash.blobs_dir.join(format!("{ref_id}.stdout"));
        fs::write(&blob, b"different\nstuff!!\n").unwrap();
        let err = stash
            .grep_lines(&ref_id, Channel::Stdout, "pattern")
            .unwrap_err();
        assert!(
            matches!(err, FoldbackError::Corrupted { .. }),
            "grep_lines on corrupted blob must return Corrupted"
        );
    }

    // ── Risk 3: explicit directory and db-file permissions ───────────────────

    /// Stash::open must explicitly chmod data_dir→0700, blobs_dir→0700,
    /// meta.db→0600.  We use a fresh subdirectory so create_dir_all actually
    /// creates it (TempDir itself might already have 0700 from the OS).
    #[test]
    fn test_data_dir_and_db_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data"); // fresh path, created by open()
        let _stash = Stash::open(&data_dir).unwrap();

        let data_mode = fs::metadata(&data_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(data_mode, 0o700, "data_dir must be 0700, got {data_mode:o}");

        let blobs_mode = fs::metadata(data_dir.join("blobs"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            blobs_mode, 0o700,
            "blobs_dir must be 0700, got {blobs_mode:o}"
        );

        let db_mode = fs::metadata(data_dir.join("meta.db"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(db_mode, 0o600, "meta.db must be 0600, got {db_mode:o}");
    }
}
