//! Spots an Indian language written in English letters ("kal subah free hai
//! kya"), which speech-to-text sometimes returns for code-mixed speech.
//! Models tend to answer such text in English, so the call switches to the
//! language before the reply (see `actor.rs`).
//!
//! The word lists hold common words of each language that are not English
//! words, so an English sentence scores zero.

/// Distinctive romanized words per language.
const WORDS: &[(&str, &[&str])] = &[
    (
        "hi-IN",
        &[
            "hai", "hain", "kya", "kyaa", "nahi", "nahin", "kal", "aaj", "parso", "aap", "aapka",
            "aapko", "mujhe", "mera", "meri", "mere", "haan", "haanji", "theek", "thik", "karo",
            "kar", "karna", "karenge", "kijiye", "dijiye", "baje", "subah", "shaam", "raat",
            "naam", "kaise", "kaisa", "accha", "acha", "achha", "bhi", "tha", "thi", "kaun", "kab",
            "kitne", "kitna", "chahiye", "abhi", "bolo", "batao", "bataiye", "sahi", "ji", "raha",
            "rahe", "rahi", "hoga", "gaya", "wala", "wali", "yeh", "woh", "aur", "lekin", "kyun",
            "kuch", "bahut", "matlab", "samay", "mein", "hum", "humko", "chalega",
        ],
    ),
    (
        "ta-IN",
        &[
            "naan",
            "enna",
            "venum",
            "vendam",
            "illa",
            "illai",
            "irukku",
            "irukka",
            "nalaiku",
            "naalaikku",
            "nalaikku",
            "inniku",
            "innaikku",
            "eppo",
            "enga",
            "sollunga",
            "pannunga",
            "panna",
            "aama",
            "aamaa",
            "ungal",
            "unga",
            "enakku",
            "theriyum",
            "theriyadhu",
            "mani",
            "kaalai",
            "saayangalam",
            "romba",
            "nandri",
            "vaanga",
            "varen",
            "varuven",
            "seringa",
            "paakalam",
            "mudiyuma",
            "mudiyum",
        ],
    ),
    (
        "te-IN",
        &[
            "nenu",
            "meeru",
            "emi",
            "enti",
            "kavali",
            "ledu",
            "undi",
            "unda",
            "repu",
            "ivvala",
            "eppudu",
            "ekkada",
            "cheppandi",
            "cheyandi",
            "chesi",
            "avunu",
            "naaku",
            "telusu",
            "teledu",
            "gantalu",
            "gantaki",
            "udayam",
            "sayantram",
            "chala",
            "dhanyavadalu",
            "randi",
            "vastanu",
            "alage",
            "sarenandi",
        ],
    ),
    (
        "kn-IN",
        &[
            "naanu",
            "neevu",
            "beku",
            "beda",
            "ide",
            "ideya",
            "naale",
            "ivattu",
            "yaavaga",
            "elli",
            "heli",
            "maadi",
            "haudu",
            "howdu",
            "nanage",
            "gottu",
            "gottilla",
            "gante",
            "ganteg",
            "beligge",
            "sanje",
            "tumba",
            "dhanyavaada",
            "banni",
            "bartini",
            "aagutta",
            "aitu",
            "aaytu",
        ],
    ),
];

/// Fewest matching words, and the share of all words that must match.
const MIN_HITS: usize = 2;
const MIN_SHARE: f32 = 0.3;

/// The language `text` is written in when it is an Indian language in
/// English letters; `None` for English, native script or unclear text.
pub fn romanized_language(text: &str) -> Option<&'static str> {
    let words: Vec<String> = text
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    if words.is_empty() {
        return None;
    }
    let mut best: Option<(&'static str, usize)> = None;
    let mut tie = false;
    for (lang, list) in WORDS {
        let hits = words.iter().filter(|w| list.contains(&w.as_str())).count();
        match best {
            Some((_, b)) if hits == b => tie = true,
            Some((_, b)) if hits < b => {}
            _ => {
                best = Some((lang, hits));
                tie = false;
            }
        }
    }
    let (lang, hits) = best?;
    let share = hits as f32 / words.len() as f32;
    (!tie && hits >= MIN_HITS && share >= MIN_SHARE).then_some(lang)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn romanized_indian_languages_are_spotted() {
        assert_eq!(
            romanized_language("haan ji, kal subah 11:30 baje free hai kya?"),
            Some("hi-IN")
        );
        assert_eq!(
            romanized_language("theek hai, book kar do, naam Rahul Sharma hai"),
            Some("hi-IN")
        );
        assert_eq!(romanized_language("haan sahi hai"), Some("hi-IN"));
        assert_eq!(
            romanized_language("nalaiku kaalai pathu mani ok va"),
            Some("ta-IN")
        );
        assert_eq!(
            romanized_language("repu udayam pade gantaki kavali"),
            Some("te-IN")
        );
        assert_eq!(
            romanized_language("naale sanje naalku gante aagutta"),
            Some("kn-IN")
        );
    }

    #[test]
    fn english_and_native_script_are_left_alone() {
        for t in [
            "Yes, tomorrow around 11 AM?",
            "My name is Lakshmi.",
            "Can you talk in Telugu?",
            "Okay so kal is fine",
            "नमस्ते",
            "",
        ] {
            assert_eq!(romanized_language(t), None, "{t}");
        }
    }
}
