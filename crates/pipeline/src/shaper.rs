//! Turns the LLM token stream into speakable clauses, enforcing the
//! response-length guardrails in code (not only in the prompt).
//!
//! - **Sanitize:** markdown symbols, list markers, emoji and newlines are
//!   removed before TTS. Number, date and ₹ normalization is left to
//!   Sarvam's `enable_preprocessing`.
//! - **Unspeakable content:** a URL, email address or long digit run
//!   (8+ digits, e.g. phone or account numbers) ends the reply at the
//!   previous clause boundary; the caller then plays the fixed
//!   "I'll send that on WhatsApp" line.
//! - **Clause chunking:** a clause ends at `.`, `?`, `!`, `।`, `॥` or a
//!   newline, or at `,` once it has at least 4 words. The boundary must
//!   be followed by whitespace, so `2.5` and `www.` never split.
//! - **Per-turn cap:** no clause is sent past `max_turn_chars`; the turn
//!   ends at the last whole clause (the first clause is cut at a word
//!   boundary if it alone is too long).
//! - **Sentence cap:** the reply ends after `max_sentences` sentences.
//!   One- or two-word acknowledgements ("Got it.") and `...` pauses don't
//!   count, so "Got it. We run ads. Want details?" is two sentences.
//! - **Per-call budget:** clauses stop when the call's remaining TTS
//!   character budget is used up.
//! - **Tone tags:** a stray `[happy]`-style tag is removed (the leading
//!   one is read and stripped earlier, in `turn.rs`).

use std::sync::LazyLock;

use regex::Regex;

/// Why the shaper stopped accepting text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// `MAX_SPOKEN_CHARS_PER_TURN` or `MAX_SENTENCES_PER_TURN` reached.
    TurnCap,
    /// `MAX_TTS_CHARS_PER_CALL` reached.
    CallBudget,
    /// A URL, email or long number was about to be spoken.
    Unspeakable,
}

/// Output of one [`Shaper::push`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Shaped {
    /// Complete clauses ready for TTS.
    pub clauses: Vec<String>,
    /// Set once the shaper stops; further input is ignored.
    pub stop: Option<StopReason>,
}

static UNSPEAKABLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:https?://|www\.)\S|[\w.+-]+@[\w-]+\.[a-z]|(?:\d[\s-]?){7}\d")
        .unwrap_or_else(|_| Regex::new("$^").unwrap_or_else(|_| unreachable!()))
});

/// Tone tags ("[happy]") that slipped past the turn's leading-tag reader.
static TONE_TAG: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\[\s*(?:neutral|happy|serious)\s*\]")
        .unwrap_or_else(|_| Regex::new("$^").unwrap_or_else(|_| unreachable!()))
});

/// Stateful shaper for one assistant turn.
#[derive(Debug)]
pub struct Shaper {
    pending: String,
    emitted_chars: usize,
    max_turn_chars: usize,
    budget_chars: usize,
    max_sentences: usize,
    sentences: usize,
    /// Words of the sentence in progress (it may span comma clauses).
    sentence_words: usize,
    stop: Option<StopReason>,
}

impl Shaper {
    /// A shaper with a per-turn cap and the call's remaining budget (in characters).
    pub fn new(max_turn_chars: usize, budget_chars: usize) -> Self {
        Self {
            pending: String::new(),
            emitted_chars: 0,
            max_turn_chars,
            budget_chars,
            max_sentences: 0,
            sentences: 0,
            sentence_words: 0,
            stop: None,
        }
    }

    /// Ends the reply after `n` sentences (0 = no limit).
    pub fn with_max_sentences(mut self, n: usize) -> Self {
        self.max_sentences = n;
        self
    }

    /// Characters emitted so far.
    pub fn emitted_chars(&self) -> usize {
        self.emitted_chars
    }

    /// Why the shaper stopped, if it did.
    pub fn stopped(&self) -> Option<StopReason> {
        self.stop
    }

    /// Adds streamed text.
    pub fn push(&mut self, text: &str) -> Shaped {
        let mut out = Shaped::default();
        if self.stop.is_some() {
            return out;
        }
        self.pending.push_str(text);

        if let Some(m) = UNSPEAKABLE.find(&self.pending) {
            // Keep whole clauses before the match, drop the rest.
            let before = self.pending[..m.start()].to_owned();
            self.pending.clear();
            let mut head = before;
            while let Some(end) = next_boundary(&head, true) {
                let clause: String = head.drain(..end).collect();
                if !self.emit(&clause, &mut out) {
                    return out;
                }
            }
            self.stop = Some(StopReason::Unspeakable);
            out.stop = self.stop;
            return out;
        }

        while let Some(end) = next_boundary(&self.pending, false) {
            let clause: String = self.pending.drain(..end).collect();
            if !self.emit(&clause, &mut out) {
                break;
            }
        }
        out
    }

    /// Ends the stream. `truncated` (finish reason `length`) drops an
    /// unfinished trailing clause unless nothing was spoken yet.
    pub fn finish(&mut self, truncated: bool) -> Shaped {
        let mut out = Shaped::default();
        if self.stop.is_some() {
            return out;
        }
        let rest = std::mem::take(&mut self.pending);
        if truncated && self.emitted_chars > 0 {
            return out;
        }
        self.emit(&rest, &mut out);
        out
    }

    /// Sanitizes and emits one clause, applying the caps. Returns `false`
    /// once stopped.
    fn emit(&mut self, raw: &str, out: &mut Shaped) -> bool {
        let clause = sanitize(raw);
        if clause.is_empty() {
            return true;
        }
        let len = clause.chars().count();
        if len > self.budget_chars.saturating_sub(self.emitted_chars) {
            self.stop = Some(StopReason::CallBudget);
            out.stop = self.stop;
            return false;
        }
        if self.emitted_chars + len > self.max_turn_chars {
            if self.emitted_chars == 0 {
                let cut = cut_at_word(&clause, self.max_turn_chars);
                if !cut.is_empty() {
                    self.emitted_chars += cut.chars().count();
                    out.clauses.push(cut);
                }
            }
            self.stop = Some(StopReason::TurnCap);
            out.stop = self.stop;
            return false;
        }
        self.emitted_chars += len;
        let pause = clause.trim_end().ends_with("..");
        if !pause {
            self.sentence_words += clause.split_whitespace().count();
        }
        let ends = ends_sentence(&clause);
        out.clauses.push(clause);
        if ends {
            let words = std::mem::take(&mut self.sentence_words);
            if words <= 2 {
                return true;
            }
            self.sentences += 1;
            if self.max_sentences > 0 && self.sentences >= self.max_sentences {
                self.stop = Some(StopReason::TurnCap);
                out.stop = self.stop;
                return false;
            }
        }
        true
    }
}

/// Whether a clause ends a sentence (not a comma break or a `...` pause).
fn ends_sentence(clause: &str) -> bool {
    let t = clause.trim_end();
    !t.ends_with("..") && t.ends_with(['.', '?', '!', '।', '॥'])
}

fn is_terminal(c: char) -> bool {
    matches!(c, '.' | '?' | '!' | '।' | '॥' | '\n')
}

/// Byte index just past the first clause boundary in `s`, if complete.
/// With `at_end`, a boundary at the very end of `s` also counts.
fn next_boundary(s: &str, at_end: bool) -> Option<usize> {
    let mut words = 0usize;
    let mut in_word = false;
    let mut iter = s.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if c.is_whitespace() {
            in_word = false;
        } else if !in_word {
            in_word = true;
            words += 1;
        }
        let candidate = is_terminal(c) || (c == ',' && words >= 4);
        if !candidate {
            continue;
        }
        let end = i + c.len_utf8();
        match iter.peek() {
            Some((_, next)) if next.is_whitespace() || c == '\n' => return Some(end),
            Some(_) => {}
            None if at_end || c == '\n' => return Some(end),
            None => {}
        }
    }
    None
}

/// Removes markdown, list markers, emoji and line breaks; collapses spaces.
pub fn sanitize(raw: &str) -> String {
    let raw = TONE_TAG.replace_all(raw, "");
    let mut cleaned = String::with_capacity(raw.len());
    for line in raw.split('\n') {
        let mut line = line.trim_start();
        // List markers and headings at line start: "- ", "* ", "• ", "1. ", "2) ", "# ".
        loop {
            let before = line;
            for marker in ["- ", "* ", "• ", "+ ", "> "] {
                if let Some(rest) = line.strip_prefix(marker) {
                    line = rest.trim_start();
                }
            }
            line = line.trim_start_matches('#').trim_start();
            let digits = line.chars().take_while(char::is_ascii_digit).count();
            if digits > 0 && digits <= 2 {
                let rest = &line[digits..];
                if let Some(r) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
                    line = r.trim_start();
                }
            }
            if line == before {
                break;
            }
        }
        for c in line.chars() {
            if matches!(c, '*' | '_' | '`' | '#' | '~' | '|' | '[' | ']' | '{' | '}') || is_emoji(c)
            {
                continue;
            }
            cleaned.push(c);
        }
        cleaned.push(' ');
    }
    let mut out = String::with_capacity(cleaned.len());
    let mut last_space = true;
    for c in cleaned.chars() {
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    out.trim().to_owned()
}

fn is_emoji(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0xFE00..=0xFE0F | 0x200D | 0x20E3)
}

/// The longest prefix of `s` with at most `max` characters, ending at a
/// word boundary.
fn cut_at_word(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let prefix: String = s.chars().take(max).collect();
    match prefix.rfind(char::is_whitespace) {
        Some(i) if i > 0 => prefix[..i].trim_end_matches([',', ' ']).to_owned(),
        _ => prefix,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(tokens: &[&str], cap: usize, budget: usize) -> (Vec<String>, Option<StopReason>) {
        let mut s = Shaper::new(cap, budget);
        let mut clauses = Vec::new();
        let mut stop = None;
        for t in tokens {
            let out = s.push(t);
            clauses.extend(out.clauses);
            stop = stop.or(out.stop);
        }
        let out = s.finish(false);
        clauses.extend(out.clauses);
        (clauses, stop.or(out.stop))
    }

    #[test]
    fn english_clauses_stream_early() {
        let mut s = Shaper::new(250, 4000);
        assert!(s.push("Sure! We help").clauses == vec!["Sure!".to_owned()]);
        let out = s.push(" clinics get more patients, through ads. Which");
        assert_eq!(
            out.clauses,
            vec!["We help clinics get more patients,", "through ads."]
        );
        assert_eq!(s.finish(false).clauses, vec!["Which"]);
    }

    #[test]
    fn short_comma_does_not_split_and_decimals_survive() {
        let (c, _) = run(&["Yes, it costs 2.5 lakh. Okay?"], 250, 4000);
        assert_eq!(c, vec!["Yes, it costs 2.5 lakh.", "Okay?"]);
    }

    #[test]
    fn sentence_cap_keeps_short_acknowledgements() {
        let mut s = Shaper::new(250, 4000).with_max_sentences(2);
        let out = s.push(
            "Got it. Hmm... we help clinics with ads. It works well, really well, for many. Want details? More text.",
        );
        assert_eq!(
            out.clauses,
            vec![
                "Got it.",
                "Hmm...",
                "we help clinics with ads.",
                "It works well, really well,",
                "for many."
            ]
        );
        assert_eq!(out.stop, Some(StopReason::TurnCap));
        assert!(s.finish(false).clauses.is_empty());
    }

    #[test]
    fn ellipsis_pause_is_its_own_clause() {
        let (c, _) = run(
            &["Hmm... let me", " think. So we can... actually, yes."],
            250,
            4000,
        );
        assert_eq!(
            c,
            vec!["Hmm...", "let me think.", "So we can...", "actually, yes."]
        );
    }

    #[test]
    fn hindi_danda_and_tamil() {
        let (c, _) = run(&["जी हाँ। हम आपकी मदद", " कर सकते हैं। क्या आप"], 250, 4000);
        assert_eq!(c, vec!["जी हाँ।", "हम आपकी मदद कर सकते हैं।", "क्या आप"]);
        let (t, _) = run(&["வணக்கம். உங்களுக்கு எப்படி உதவலாம்?"], 250, 4000);
        assert_eq!(t, vec!["வணக்கம்.", "உங்களுக்கு எப்படி உதவலாம்?"]);
    }

    #[test]
    fn strips_markdown_lists_and_emoji() {
        assert_eq!(sanitize("**Great** news 🎉"), "Great news");
        assert_eq!(
            sanitize("- first item\n2. second\n# Title"),
            "first item second Title"
        );
        assert_eq!(sanitize("`code` and _x_"), "code and x");
    }

    #[test]
    fn url_email_and_phone_stop_the_reply() {
        let (c, stop) = run(&["Sure. Visit www.example.com for more."], 250, 4000);
        assert_eq!(c, vec!["Sure."]);
        assert_eq!(stop, Some(StopReason::Unspeakable));

        let (c, stop) = run(&["Okay. Mail us at team@clinic.in today."], 250, 4000);
        assert_eq!(
            (c, stop),
            (vec!["Okay.".to_owned()], Some(StopReason::Unspeakable))
        );

        let (c, stop) = run(&["Call 98765 43210 anytime."], 250, 4000);
        assert!(c.is_empty());
        assert_eq!(stop, Some(StopReason::Unspeakable));
    }

    #[test]
    fn prices_are_not_unspeakable() {
        let (c, stop) = run(&["It costs ₹1,50,000 or 150000 rupees."], 250, 4000);
        assert_eq!(c.len(), 1);
        assert_eq!(stop, None);
    }

    #[test]
    fn turn_cap_cuts_at_sentence_boundary() {
        let (c, stop) = run(
            &["First sentence here. Second sentence is a bit longer. Third one."],
            50,
            4000,
        );
        assert_eq!(c, vec!["First sentence here."]);
        assert_eq!(stop, Some(StopReason::TurnCap));
    }

    #[test]
    fn overlong_first_clause_is_cut_at_a_word() {
        let long = "word ".repeat(80);
        let (c, stop) = run(&[&long], 30, 4000);
        assert_eq!(c.len(), 1);
        assert!(c[0].chars().count() <= 30);
        assert!(!c[0].ends_with(' '));
        assert_eq!(stop, Some(StopReason::TurnCap));
    }

    #[test]
    fn call_budget_stops_output() {
        let (c, stop) = run(&["One two three. Four five six seven."], 250, 20);
        assert_eq!(c, vec!["One two three."]);
        assert_eq!(stop, Some(StopReason::CallBudget));
    }

    #[test]
    fn truncated_finish_drops_partial_clause() {
        let mut s = Shaper::new(250, 4000);
        s.push("Done. And then the");
        assert!(s.finish(true).clauses.is_empty());
        let mut s = Shaper::new(250, 4000);
        s.push("only partial");
        assert_eq!(s.finish(true).clauses, vec!["only partial"]);
    }

    #[test]
    fn stray_tone_tags_are_removed() {
        assert_eq!(sanitize("Done. [Happy] See you!"), "Done. See you!");
    }
}
