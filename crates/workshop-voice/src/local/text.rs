//! Segment text hygiene: whisper emits pseudo-tokens (`[BLANK_AUDIO]`, `(applause)`, `♪`) on
//! silence and non-speech; none of those belong in a prompt box.

/// Strip non-speech markers and collapse whitespace. Returns `""` when nothing speakable is left.
pub fn clean_segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut depth_square = 0usize;
    let mut depth_round = 0usize;
    for ch in raw.chars() {
        match ch {
            '[' => depth_square += 1,
            ']' => depth_square = depth_square.saturating_sub(1),
            '(' => depth_round += 1,
            ')' => depth_round = depth_round.saturating_sub(1),
            '♪' | '♫' => {}
            _ if depth_square > 0 || depth_round > 0 => {}
            _ => out.push(ch),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::clean_segment;

    #[test]
    fn drops_markers_keeps_words() {
        assert_eq!(clean_segment(" [BLANK_AUDIO]"), "");
        assert_eq!(clean_segment("(applause) thank you"), "thank you");
        assert_eq!(clean_segment("♪ la la ♪"), "la la");
        assert_eq!(
            clean_segment("  And so,  my fellow Americans. "),
            "And so, my fellow Americans."
        );
    }
}
