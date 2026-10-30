//! Token-aware chunking.
//!
//! Text is split into sentences (`.`, `?`, `!`, `।`, blank lines), then
//! sentences are packed into chunks of about `target` tokens. The last
//! sentences of each chunk (up to `overlap` tokens) are repeated at the
//! start of the next so answers spanning a boundary are still retrievable.
//! Sentences longer than `target` are split on word boundaries.

/// One chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Text.
    pub content: String,
    /// Token count (by the supplied counter).
    pub token_count: usize,
}

/// Splits text into sentences, keeping the terminator.
pub fn sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' && chars.peek() == Some(&'\n') {
            push_trimmed(&mut out, &mut current);
            continue;
        }
        current.push(if c == '\n' { ' ' } else { c });
        let terminal = matches!(c, '.' | '?' | '!' | '।' | '॥');
        if terminal && chars.peek().is_none_or(|n| n.is_whitespace()) {
            push_trimmed(&mut out, &mut current);
        }
    }
    push_trimmed(&mut out, &mut current);
    out
}

fn push_trimmed(out: &mut Vec<String>, current: &mut String) {
    let s = current.split_whitespace().collect::<Vec<_>>().join(" ");
    if !s.is_empty() {
        out.push(s);
    }
    current.clear();
}

/// Chunks `text` into pieces of about `target` tokens with `overlap`
/// tokens of carry-over, using `count` to measure tokens.
pub fn chunk_text(
    text: &str,
    target: usize,
    overlap: usize,
    count: impl Fn(&str) -> usize,
) -> Vec<Chunk> {
    // Long sentences become several word-split pieces.
    let mut pieces: Vec<(String, usize)> = Vec::new();
    for s in sentences(text) {
        let n = count(&s);
        if n <= target {
            pieces.push((s, n));
            continue;
        }
        let mut part = String::new();
        for word in s.split_whitespace() {
            let candidate = if part.is_empty() {
                word.to_owned()
            } else {
                format!("{part} {word}")
            };
            if count(&candidate) > target && !part.is_empty() {
                let c = count(&part);
                pieces.push((std::mem::take(&mut part), c));
                part = word.to_owned();
            } else {
                part = candidate;
            }
        }
        if !part.is_empty() {
            let c = count(&part);
            pieces.push((part, c));
        }
    }

    let mut chunks = Vec::new();
    let mut current: Vec<(String, usize)> = Vec::new();
    let mut tokens = 0;
    let mut fresh = 0; // pieces in `current` not carried over from the previous chunk
    for (piece, n) in pieces {
        if tokens + n > target && fresh > 0 {
            chunks.push(make_chunk(&current, tokens));
            // Carry trailing pieces (up to `overlap` tokens) into the next chunk.
            let mut carry = Vec::new();
            let mut carried = 0;
            for (p, c) in current.iter().rev() {
                if carried + c > overlap {
                    break;
                }
                carried += c;
                carry.push((p.clone(), *c));
            }
            carry.reverse();
            current = carry;
            tokens = carried;
            fresh = 0;
        }
        if fresh == 0 && tokens + n > target {
            // The overlap would push this chunk over the target: drop it.
            current.clear();
            tokens = 0;
        }
        tokens += n;
        fresh += 1;
        current.push((piece, n));
    }
    if fresh > 0 {
        chunks.push(make_chunk(&current, tokens));
    }
    chunks
}

fn make_chunk(pieces: &[(String, usize)], tokens: usize) -> Chunk {
    Chunk {
        content: pieces
            .iter()
            .map(|(p, _)| p.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        token_count: tokens,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> usize {
        s.split_whitespace().count()
    }

    #[test]
    fn splits_sentences_in_several_scripts() {
        let s = sentences("We run ads. Prices start at 2.5 lakh!\n\nहम मदद करते हैं। ठीक है?");
        assert_eq!(
            s,
            vec![
                "We run ads.",
                "Prices start at 2.5 lakh!",
                "हम मदद करते हैं।",
                "ठीक है?"
            ]
        );
    }

    #[test]
    fn packs_with_overlap_and_bounds() {
        let text = (0..40)
            .map(|i| format!("Sentence number {i} has six words."))
            .collect::<Vec<_>>()
            .join(" ");
        let chunks = chunk_text(&text, 30, 12, words);
        assert!(chunks.len() > 5);
        for c in &chunks {
            assert!(c.token_count <= 30, "{c:?}");
        }
        // Consecutive chunks share their boundary sentence(s).
        let last_of_first = sentences(&chunks[0].content).pop().unwrap();
        assert!(chunks[1].content.contains(&last_of_first));
        assert!(!chunks[1]
            .content
            .starts_with(&sentences(&chunks[0].content)[0]));
        // Every sentence appears somewhere.
        assert!(chunks.last().unwrap().content.contains("number 39"));
    }

    #[test]
    fn long_sentence_is_word_split() {
        let long = "word ".repeat(100);
        let chunks = chunk_text(&long, 30, 0, words);
        assert!(chunks.len() >= 4);
        assert!(chunks.iter().all(|c| c.token_count <= 30));
    }
}
