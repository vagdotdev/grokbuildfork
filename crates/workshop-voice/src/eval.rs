//! Word-error-rate helpers for the e2e test and the bench example. Not used at runtime.

/// Lowercase, strip punctuation, split on whitespace.
pub fn normalize_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric() || *c == '\'')
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Word error rate: Levenshtein distance over normalized words divided by reference length.
pub fn wer(reference: &str, hypothesis: &str) -> f32 {
    let r = normalize_words(reference);
    let h = normalize_words(hypothesis);
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    let mut prev: Vec<usize> = (0..=h.len()).collect();
    let mut cur = vec![0usize; h.len() + 1];
    for (i, rw) in r.iter().enumerate() {
        if let Some(c0) = cur.first_mut() {
            *c0 = i + 1;
        }
        for (j, hw) in h.iter().enumerate() {
            let sub = prev.get(j).copied().unwrap_or(0) + usize::from(rw != hw);
            let del = prev.get(j + 1).copied().unwrap_or(0) + 1;
            let ins = cur.get(j).copied().unwrap_or(0) + 1;
            if let Some(c) = cur.get_mut(j + 1) {
                *c = sub.min(del).min(ins);
            }
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev.last().copied().unwrap_or(0) as f32 / r.len() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wer_basics() {
        assert_eq!(wer("hello world", "Hello, world!"), 0.0);
        assert_eq!(wer("a b c d", "a b x d"), 0.25);
        assert_eq!(wer("a b c d", "a b d"), 0.25);
        assert_eq!(wer("a b", "a b c d"), 1.0);
        assert_eq!(wer("", ""), 0.0);
    }
}
