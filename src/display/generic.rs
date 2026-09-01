use crate::condenser::{CONDENSE_BYTE_THRESHOLD, CONDENSE_LINE_THRESHOLD};

/// Return `true` when `data` exceeds the threshold that triggers condensing.
///
/// Uses the same constants as `condenser::condense` to guarantee identical
/// threshold semantics; the actual condensing is always delegated to
/// `condenser::condense` (never-worse + marker construction stay there).
pub fn exceeds_threshold(data: &[u8]) -> bool {
    data.len() > CONDENSE_BYTE_THRESHOLD || line_count(data) > CONDENSE_LINE_THRESHOLD
}

fn line_count(data: &[u8]) -> usize {
    if data.is_empty() {
        return 0;
    }
    data.iter().filter(|&&b| b == b'\n').count() + if data.last() != Some(&b'\n') { 1 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::condenser::{CONDENSE_BYTE_THRESHOLD, CONDENSE_LINE_THRESHOLD};

    fn numbered_lines(count: usize) -> Vec<u8> {
        (0..count)
            .flat_map(|i| format!("line-{i:04}\n").into_bytes())
            .collect()
    }

    #[test]
    fn test_exceeds_threshold_line_trigger() {
        assert!(!exceeds_threshold(&numbered_lines(CONDENSE_LINE_THRESHOLD)));
        assert!(exceeds_threshold(&numbered_lines(
            CONDENSE_LINE_THRESHOLD + 1
        )));
    }

    #[test]
    fn test_exceeds_threshold_byte_trigger() {
        let under = vec![b'x'; CONDENSE_BYTE_THRESHOLD];
        let over = vec![b'x'; CONDENSE_BYTE_THRESHOLD + 1];
        assert!(!exceeds_threshold(&under));
        assert!(exceeds_threshold(&over));
    }

    #[test]
    fn test_exceeds_threshold_empty() {
        assert!(!exceeds_threshold(b""));
    }
}
