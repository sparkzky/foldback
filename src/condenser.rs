use chrono::{DateTime, Utc};

/// Lines / bytes above which we attempt condensing.
pub const CONDENSE_LINE_THRESHOLD: usize = 100;
pub const CONDENSE_BYTE_THRESHOLD: usize = 10 * 1024; // 10 KB
pub const HEAD_LINES: usize = 20;
pub const TAIL_LINES: usize = 20;

pub struct CondenseResult {
    /// Condensed output to print to the terminal (valid UTF-8 when condensed;
    /// passthrough may contain invalid UTF-8 bytes).
    pub display: Vec<u8>,
    /// True if condensing was applied (i.e. display != original).
    pub condensed: bool,
}

/// Condense `data` for terminal display.
///
/// Returns `CondenseResult` where `.condensed` is true only when the output
/// exceeded the threshold AND the condensed form is strictly smaller.
///
/// Raw storage happens outside this module; we only produce the display form.
pub fn condense(data: &[u8], ref_id: &str, expires_at: &DateTime<Utc>) -> CondenseResult {
    let exceeds_threshold =
        data.len() > CONDENSE_BYTE_THRESHOLD || line_count(data) > CONDENSE_LINE_THRESHOLD;

    if !exceeds_threshold {
        return CondenseResult {
            display: data.to_vec(),
            condensed: false,
        };
    }

    let condensed = build_condensed(data, ref_id, expires_at);
    if condensed.len() >= data.len() {
        // Condensing doesn't help — return original bytes (may be non-UTF-8)
        return CondenseResult {
            display: data.to_vec(),
            condensed: false,
        };
    }

    CondenseResult {
        display: condensed,
        condensed: true,
    }
}

fn line_count(data: &[u8]) -> usize {
    if data.is_empty() {
        return 0;
    }
    data.iter().filter(|&&b| b == b'\n').count() + if data.last() != Some(&b'\n') { 1 } else { 0 }
}

fn build_condensed(data: &[u8], ref_id: &str, expires_at: &DateTime<Utc>) -> Vec<u8> {
    let lines: Vec<&[u8]> = split_lines(data);
    let total_lines = lines.len();
    let raw_bytes = data.len();

    let head_count = HEAD_LINES.min(total_lines);
    let tail_count = TAIL_LINES.min(total_lines.saturating_sub(head_count));
    let omitted = total_lines.saturating_sub(head_count + tail_count);

    let marker = format!(
        "[rawref ref={ref_id} raw={raw_bytes}b lines={total_lines} omitted={omitted} expires={}]\n",
        expires_at.format("%Y-%m-%dT%H:%M:%SZ")
    );

    let mut out = Vec::with_capacity(raw_bytes / 2 + marker.len());

    for line in &lines[..head_count] {
        out.extend_from_slice(line);
        if !line.ends_with(b"\n") {
            out.push(b'\n');
        }
    }
    out.extend_from_slice(marker.as_bytes());
    if tail_count > 0 {
        let tail_start = total_lines - tail_count;
        for line in &lines[tail_start..] {
            out.extend_from_slice(line);
            if !line.ends_with(b"\n") {
                out.push(b'\n');
            }
        }
    }

    out
}

/// Split `data` into lines, preserving trailing `\n` on each line.
fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    for i in 0..data.len() {
        if data[i] == b'\n' {
            lines.push(&data[start..=i]);
            start = i + 1;
        }
    }
    if start < data.len() {
        lines.push(&data[start..]);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const REF_ID: &str = "abc123";

    fn expires() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 7, 0, 0, 0).unwrap()
    }

    /// Build `count` wide lines (>400 bytes each) so total exceeds 10 KB.
    fn wide_numbered_lines(count: usize) -> Vec<u8> {
        let padding = "x".repeat(500);
        (0..count)
            .flat_map(|i| format!("line-{i:04} {padding}\n").into_bytes())
            .collect()
    }

    /// Build `count` lines of `line-{i:04}\n`, optionally dropping the final `\n`.
    fn numbered_lines(count: usize, trailing_newline: bool) -> Vec<u8> {
        let mut data: Vec<u8> = (0..count)
            .flat_map(|i| format!("line-{i:04}\n").into_bytes())
            .collect();
        if !trailing_newline && data.last() == Some(&b'\n') {
            data.pop();
        }
        data
    }

    fn byte_blob(size: usize) -> Vec<u8> {
        vec![b'x'; size]
    }

    #[test]
    fn test_short_output_passthrough() {
        let data = b"hello world\n";
        let res = condense(data, REF_ID, &expires());
        assert!(!res.condensed);
        assert_eq!(res.display, data);
    }

    #[test]
    fn test_long_output_condenses() {
        let data = numbered_lines(200, true);
        let res = condense(&data, REF_ID, &expires());
        assert!(res.condensed);
        assert!(res.display.len() < data.len());
        assert!(res.display.windows(9).any(|w| w == b"[rawref r"));
    }

    #[test]
    fn test_condensed_contains_ref_marker() {
        let data = numbered_lines(200, true);
        let res = condense(&data, "myrefid", &expires());
        assert!(res.condensed);
        let display_str = String::from_utf8_lossy(&res.display);
        assert!(display_str.contains("ref=myrefid"));
        assert!(display_str.contains("expires="));
    }

    #[test]
    fn test_exactly_100_lines_passthrough() {
        let data = numbered_lines(CONDENSE_LINE_THRESHOLD, true);
        assert_eq!(line_count(&data), CONDENSE_LINE_THRESHOLD);
        assert!(data.len() <= CONDENSE_BYTE_THRESHOLD);

        let res = condense(&data, REF_ID, &expires());
        assert!(!res.condensed);
        assert_eq!(res.display, data);
    }

    #[test]
    fn test_101_lines_condenses() {
        let data = numbered_lines(CONDENSE_LINE_THRESHOLD + 1, true);
        assert_eq!(line_count(&data), CONDENSE_LINE_THRESHOLD + 1);

        let res = condense(&data, REF_ID, &expires());
        assert!(res.condensed);
        assert!(res.display.len() < data.len());
    }

    #[test]
    fn test_byte_threshold_at_exactly_10kb_passthrough() {
        let data = byte_blob(CONDENSE_BYTE_THRESHOLD);
        assert_eq!(data.len(), CONDENSE_BYTE_THRESHOLD);
        assert_eq!(line_count(&data), 1);

        let res = condense(&data, REF_ID, &expires());
        assert!(!res.condensed);
        assert_eq!(res.display, data);
    }

    #[test]
    fn test_byte_threshold_one_below_passthrough() {
        let data = byte_blob(CONDENSE_BYTE_THRESHOLD - 1);
        assert_eq!(line_count(&data), 1);

        let res = condense(&data, REF_ID, &expires());
        assert!(!res.condensed);
        assert_eq!(res.display, data);
    }

    #[test]
    fn test_byte_threshold_one_above_passthrough_when_no_space_saved() {
        let data = byte_blob(CONDENSE_BYTE_THRESHOLD + 1);
        assert_eq!(line_count(&data), 1);

        let res = condense(&data, REF_ID, &expires());
        assert!(!res.condensed);
        assert_eq!(res.display, data);
    }

    #[test]
    fn test_single_line_over_10kb_passthrough() {
        let data = byte_blob(CONDENSE_BYTE_THRESHOLD + 2048);
        assert_eq!(line_count(&data), 1);
        assert!(data.len() > CONDENSE_BYTE_THRESHOLD);

        let res = condense(&data, REF_ID, &expires());
        assert!(!res.condensed);
        assert_eq!(res.display, data);
    }

    #[test]
    fn test_no_trailing_newline_still_condenses() {
        let data = numbered_lines(200, false);
        assert_eq!(line_count(&data), 200);
        assert_ne!(data.last(), Some(&b'\n'));

        let res = condense(&data, REF_ID, &expires());
        assert!(res.condensed);
        assert!(res.display.len() < data.len());
    }

    #[test]
    fn test_byte_threshold_with_few_lines_preserves_head_and_tail() {
        let total = 25;
        let data = wide_numbered_lines(total);
        assert!(data.len() > CONDENSE_BYTE_THRESHOLD);
        assert_eq!(line_count(&data), total);

        // Buggy path (head + marker, tail dropped) would be strictly smaller — RED fixture.
        let head_lines_bytes: usize = data
            .split(|&b| b == b'\n')
            .take(HEAD_LINES)
            .map(|l| l.len() + 1)
            .sum();
        let marker_len = format!(
            "[rawref ref={REF_ID} raw={}b lines={total} omitted=0 expires={}]\n",
            data.len(),
            expires().format("%Y-%m-%dT%H:%M:%SZ")
        )
        .len();
        assert!(
            head_lines_bytes + marker_len < data.len(),
            "fixture must compress when tail is wrongly dropped"
        );

        // Fixture exceeds byte threshold with omitted=0 (head+tail covers all lines).
        // RED: buggy build_condensed dropped tail despite tail_count>0.
        let built = build_condensed(&data, REF_ID, &expires());
        let display = String::from_utf8_lossy(&built);
        for i in 0..HEAD_LINES {
            assert!(
                display.contains(&format!("line-{i:04}")),
                "missing head line {i}"
            );
        }
        for i in HEAD_LINES..total {
            assert!(
                display.contains(&format!("line-{i:04}")),
                "missing tail line {i} (omitted=0 must not drop tail)"
            );
        }
        assert!(display.contains("omitted=0"));

        // Keeping all 25 lines + marker has no benefit at condense layer → passthrough.
        let res = condense(&data, REF_ID, &expires());
        assert!(!res.condensed);
        assert_eq!(res.display, data);
    }

    #[test]
    fn test_head_and_tail_lines_preserved() {
        let total = 200;
        let data = numbered_lines(total, true);
        let res = condense(&data, REF_ID, &expires());
        assert!(res.condensed);

        let display = String::from_utf8_lossy(&res.display);
        for i in 0..HEAD_LINES {
            assert!(
                display.contains(&format!("line-{i:04}")),
                "missing head line {i}"
            );
        }
        for i in (total - TAIL_LINES)..total {
            assert!(
                display.contains(&format!("line-{i:04}")),
                "missing tail line {i}"
            );
        }

        let middle = HEAD_LINES + 50;
        assert!(
            !display.contains(&format!("line-{middle:04}")),
            "middle line {middle} should be omitted"
        );

        let omitted = total - HEAD_LINES - TAIL_LINES;
        assert!(display.contains(&format!("omitted={omitted}")));
    }

    #[test]
    fn test_no_space_saved_passthrough_preserves_original_bytes() {
        let mut data = byte_blob(CONDENSE_BYTE_THRESHOLD + 100);
        data[0] = 0xFF;
        data[1] = 0xFE;

        let res = condense(&data, REF_ID, &expires());
        assert!(!res.condensed);
        assert_eq!(res.display, data);
    }

    #[test]
    fn test_line_count_empty() {
        assert_eq!(line_count(b""), 0);
    }

    #[test]
    fn test_line_count_no_trailing_newline() {
        assert_eq!(line_count(b"a\nb"), 2);
    }

    #[test]
    fn test_line_count_trailing_newline() {
        assert_eq!(line_count(b"a\nb\n"), 2);
    }
}
