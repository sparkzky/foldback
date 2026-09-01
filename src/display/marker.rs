use chrono::{DateTime, Utc};

fn line_count(data: &[u8]) -> usize {
    if data.is_empty() {
        return 0;
    }
    data.iter().filter(|&&b| b == b'\n').count() + if data.last() != Some(&b'\n') { 1 } else { 0 }
}

/// Build the **specialised** foldback marker for a non-generic reducer output.
///
/// Format:
/// ```text
/// [foldback ref=<32hex> raw=<bytes>b lines=<n> view=<view> mode=summary
///  recoverability=retrievable expires=<ISO8601Z>]
/// ```
///
/// Constraints (from Phase 2 marker contract):
/// - Contains `raw=`, `lines=`, `view=`, `mode=summary`, `recoverability=retrievable`, `expires=`.
/// - Does **NOT** contain `omitted=` (semantic recomposition makes the count unverifiable).
/// - `raw=` / `lines=` are calculated from `raw_bytes` (the original capture), not the reduced view.
pub fn build_specialized_marker(
    ref_id: &str,
    raw: &[u8],
    view: &str,
    expires_at: &DateTime<Utc>,
) -> Vec<u8> {
    let raw_bytes = raw.len();
    let total_lines = line_count(raw);
    format!(
        "[foldback ref={ref_id} raw={raw_bytes}b lines={total_lines} view={view} mode=summary recoverability=retrievable expires={}]\n",
        expires_at.format("%Y-%m-%dT%H:%M:%SZ")
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn expires() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 7, 0, 0, 0).unwrap()
    }

    fn raw_200_lines() -> Vec<u8> {
        (0..200)
            .flat_map(|i| format!("line-{i:04}\n").into_bytes())
            .collect()
    }

    #[test]
    fn test_specialized_marker_contains_required_fields() {
        let raw = raw_200_lines();
        let marker = build_specialized_marker(
            "abc123def456abc123def456abc123de",
            &raw,
            "pytest",
            &expires(),
        );
        let s = String::from_utf8(marker).expect("marker must be UTF-8");

        // Must contain these fields
        assert!(
            s.contains("ref=abc123def456abc123def456abc123de"),
            "missing ref="
        );
        assert!(s.contains("view=pytest"), "missing view=");
        assert!(s.contains("mode=summary"), "missing mode=summary");
        assert!(
            s.contains("recoverability=retrievable"),
            "missing recoverability="
        );
        assert!(s.contains("expires="), "missing expires=");
        assert!(s.contains(&format!("raw={}b", raw.len())), "missing raw=");
        assert!(s.contains("lines=200"), "missing lines=200");
    }

    #[test]
    fn test_specialized_marker_no_omitted_field() {
        let raw = raw_200_lines();
        let marker = build_specialized_marker(
            "abc123def456abc123def456abc123de",
            &raw,
            "pytest",
            &expires(),
        );
        let s = String::from_utf8(marker).expect("marker must be UTF-8");
        assert!(
            !s.contains("omitted="),
            "specialized marker must NOT contain omitted="
        );
    }

    #[test]
    fn test_specialized_marker_cargo_test_view() {
        let raw = raw_200_lines();
        let marker = build_specialized_marker(
            "abc123def456abc123def456abc123de",
            &raw,
            "cargo-test",
            &expires(),
        );
        let s = String::from_utf8(marker).expect("marker must be UTF-8");
        assert!(
            s.contains("view=cargo-test"),
            "must contain view=cargo-test"
        );
        assert!(
            !s.contains("omitted="),
            "specialized marker must NOT contain omitted="
        );
    }

    #[test]
    fn test_specialized_marker_starts_with_foldback_prefix() {
        let raw = raw_200_lines();
        let marker = build_specialized_marker(
            "abc123def456abc123def456abc123de",
            &raw,
            "pytest",
            &expires(),
        );
        let s = String::from_utf8(marker).expect("marker must be UTF-8");
        assert!(
            s.starts_with("[foldback ref="),
            "marker must start with [foldback ref="
        );
    }

    #[test]
    fn test_specialized_marker_ends_with_newline() {
        let raw = raw_200_lines();
        let marker = build_specialized_marker(
            "abc123def456abc123def456abc123de",
            &raw,
            "pytest",
            &expires(),
        );
        assert!(marker.ends_with(b"\n"), "marker must end with newline");
    }

    #[test]
    fn test_specialized_marker_extract_ref_id_compatible() {
        // The `extract_ref_id()` helper in integration tests uses `split("ref=")` then
        // grabs 32 hex chars.  Verify the marker is compatible.
        let raw = raw_200_lines();
        let ref_id = "aabbccddeeff00112233445566778899";
        let marker = build_specialized_marker(ref_id, &raw, "pytest", &expires());
        let s = String::from_utf8(marker).expect("marker must be UTF-8");
        // Simulate the helper logic
        let extracted = s
            .split("ref=")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .map(|s| &s[..32.min(s.len())]);
        assert_eq!(
            extracted,
            Some(ref_id),
            "extract_ref_id must succeed on specialized marker"
        );
    }

    #[test]
    fn test_line_count_helper() {
        assert_eq!(line_count(b""), 0);
        assert_eq!(line_count(b"a\nb\n"), 2);
        assert_eq!(line_count(b"a\nb"), 2);
        assert_eq!(line_count(b"a"), 1);
    }
}
