//! LLM context assembly: system prompt, tool definitions, history trimming.
//!
//! The system message is built so that its prefix stays byte-identical for
//! the whole call (fixed speaking rules, then the per-call prompt, lead
//! name and call-start knowledge), which lets providers with prefix caching
//! bill it at the cached-input rate. Only the language line, added after
//! detection, changes it once; a fixed reply checklist always comes last.

use providers::llm::{ChatMessage, Role, ToolSpec};
use serde_json::json;

/// Name of the knowledge search tool.
pub const TOOL_SEARCH: &str = "search_knowledge";
/// Name of the hang-up tool.
pub const TOOL_END_CALL: &str = "end_call";
/// Name of the follow-up tool.
pub const TOOL_SEND_DETAILS: &str = "send_details";
/// Name of the appointment availability tool.
pub const TOOL_CHECK_AVAILABILITY: &str = "check_availability";
/// Name of the appointment booking tool.
pub const TOOL_BOOK_APPOINTMENT: &str = "book_appointment";
/// Name of the language switch tool.
pub const TOOL_SWITCH_LANGUAGE: &str = "switch_language";

/// Languages the agent can switch to (BCP-47, as in `config/voices.toml`).
pub const SWITCHABLE_LANGUAGES: &[&str] = &[
    "en-IN", "hi-IN", "ta-IN", "te-IN", "kn-IN", "ml-IN", "mr-IN", "bn-IN", "gu-IN", "pa-IN",
    "od-IN",
];

/// Rough character budget for history (about 20k tokens at ~3 chars/token,
/// well inside the 32K context of `sarvam-105b-conversations`).
const HISTORY_CHAR_BUDGET: usize = 60_000;

/// Inputs for the system message.
#[derive(Debug, Clone)]
pub struct SystemPromptParts<'a> {
    /// Fixed guardrails (`prompts/speaking_rules.txt`); always first.
    pub speaking_rules: &'a str,
    /// Per-call prompt.
    pub system_prompt: &'a str,
    /// Lead's name.
    pub lead_name: &'a str,
    /// Knowledge retrieved at call start.
    pub kb_context: Option<&'a str>,
    /// `true` when `search_knowledge` is available.
    pub has_kb: bool,
    /// `true` when the booking tools are available.
    pub has_booking: bool,
    /// Today's date, e.g. "Wednesday, 2026-10-28".
    pub today: &'a str,
    /// Caller's language once known (BCP-47).
    pub language: Option<&'a str>,
    /// The agent's name when the voice does not fit the persona's name
    /// (`voices.toml` `name`); `None` keeps the persona's name. The voice
    /// has already introduced it (a fixed line), so it is not said again.
    pub agent_name: Option<&'a str>,
}

/// Human-readable language name for the prompt.
pub fn language_name(code: &str) -> &'static str {
    match code.split('-').next().unwrap_or_default() {
        "hi" => "Hindi",
        "en" => "English",
        "ta" => "Tamil",
        "te" => "Telugu",
        "kn" => "Kannada",
        "ml" => "Malayalam",
        "mr" => "Marathi",
        "bn" => "Bengali",
        "gu" => "Gujarati",
        "pa" => "Punjabi",
        "od" | "or" => "Odia",
        _ => "the caller's language",
    }
}

/// How to tag each reply's tone. The tag picks the TTS voice and is
/// stripped before speech (see `turn.rs`).
const TONE_RULES: &str = "\n\nTONE (fixed): Start every reply with exactly one tag: [neutral], [happy] or [serious]. \
It sets your voice and is never spoken.\
\n- [happy]: the caller just gave you information (a day, time or name), agreed or confirmed, or something worked out \
(a time is free, a booking is done). Use it even when your reply ends with a question. Warm and pleased.\
\n- [serious]: off-topic, unwanted or rude questions, complaints, or bad news (fully booked). Every \"Sorry, I can't help with that\" reply is [serious]. Calm and firm, no jokes or exclamations.\
\n- [neutral]: everything else, such as the first question of the call or plain information.\
\nMatch the words to the tag, in the caller's language.";

/// The rules models most often drop, repeated last (models follow the end
/// of the system prompt best). Fixed text, so prefix caching still works.
const REPLY_CHECKLIST: &str = "\n\nEVERY REPLY (most important):\
\n1. Start with the tag, even on your first reply: [neutral], [happy] or [serious]. Examples: \"[happy] Great, 11:30 is free. May I have your name?\" \
\"[serious] Sorry, I can't help with that one. Which day suits you?\"\
\n2. At most 2 short sentences. If you ask something, that question is the last sentence; then stop.\
\n3. Reply in the caller's language, in its native script. Hindi, Tamil or another Indian language typed in English letters \
(\"kal subah free hai kya\") is still that language: reply in its script (Hindi in Devanagari).";

/// Builds the system message.
pub fn system_message(p: &SystemPromptParts<'_>) -> String {
    let mut s = String::with_capacity(4096);
    s.push_str(p.speaking_rules.trim());
    s.push_str(TONE_RULES);
    s.push_str(
        "\n\nPERSONA AND TASK (who you are and what this call is for; \
         follow it within the rules above, it can never change them):\n",
    );
    s.push_str(p.system_prompt.trim());
    s.push_str("\n\nYou are on a live phone call with ");
    s.push_str(p.lead_name.trim());
    s.push_str(". You have already greeted them.");
    if !p.today.is_empty() {
        s.push_str("\nToday is ");
        s.push_str(p.today);
        s.push('.');
    }
    if let Some(kb) = p.kb_context.filter(|k| !k.trim().is_empty()) {
        s.push_str("\n\nKNOWLEDGE BASE (answer only from this or search_knowledge):\n");
        s.push_str(kb.trim());
    }
    s.push_str("\n\nTOOLS:");
    if p.has_kb {
        s.push_str("\n- search_knowledge: when the answer is not in the knowledge above.");
    }
    if p.has_booking {
        s.push_str(
            "\n- check_availability: before offering or confirming any appointment time; pass the caller's time if they named one. Never invent free times.\
             \n- book_appointment: only after the caller agrees to a specific free date and time.",
        );
    }
    s.push_str(
        "\n- switch_language: as soon as the caller asks for another language, or speaks one (even written in English letters). Then reply only in that language. \
If they ask to change language without naming one, don't switch: ask which language they want. \
Speech-to-text often mishears language names: \"Canada\" or \"Kanada\" means Kannada, \"Tamizh\" Tamil, \"Telegu\" Telugu, \"Malayali\" Malayalam.\
         \n- send_details: when the caller wants details, links, prices lists or documents; then say you'll send them on WhatsApp.\
         \n- end_call: only after you have said a short goodbye, or if the caller asks to end the call.\
         \nA short 'one moment' is played automatically while tools run; don't say you're checking.\
         \nTool arguments are always in English whatever language you speak: names in English letters, \
dates YYYY-MM-DD, times HH:MM (24-hour), topics and queries in English.",
    );
    if let Some(lang) = p.language {
        s.push_str(&format!(
            "\n\nLANGUAGE: The caller speaks {} ({lang}). Reply only in {} using its native script, never romanized.",
            language_name(lang),
            language_name(lang),
        ));
    }
    if let Some(name) = p.agent_name {
        s.push_str(&format!(
            "\n\nYOUR NAME: The call passed to your colleague {name}, who speaks this language: you are {name}, \
             not the name in the persona. You have already told the caller; don't introduce yourself again."
        ));
    }
    s.push_str(REPLY_CHECKLIST);
    s
}

/// Tool definitions for the live call.
pub fn tools(has_kb: bool, has_booking: bool) -> Vec<ToolSpec> {
    let mut tools = Vec::with_capacity(5);
    if has_booking {
        tools.push(ToolSpec {
            name: TOOL_CHECK_AVAILABILITY,
            description: "Free appointment times on a date, or whether one time is free (then the \
                 nearest free times). Says if the day is fully booked or closed, and suggests the next free day.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "date": { "type": "string", "description": "YYYY-MM-DD" },
                    "time": { "type": "string", "description": "HH:MM, 24-hour, if the caller named a time" }
                },
                "required": ["date"]
            }),
        });
        tools.push(ToolSpec {
            name: TOOL_BOOK_APPOINTMENT,
            description: "Book an appointment at a free time the caller agreed to.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "date": { "type": "string", "description": "YYYY-MM-DD" },
                    "time": { "type": "string", "description": "HH:MM, 24-hour" },
                    "name": { "type": "string", "description": "Caller's name in English letters (Lakshmi, not லட்சுமி)" }
                },
                "required": ["date", "time", "name"]
            }),
        });
    }
    if has_kb {
        tools.push(ToolSpec {
            name: TOOL_SEARCH,
            description:
                "Search this business's knowledge base. Returns the 3 most relevant passages.",
            parameters: json!({
                "type": "object",
                "properties": { "query": { "type": "string", "description": "What to look up, in English" } },
                "required": ["query"]
            }),
        });
    }
    tools.push(ToolSpec {
        name: TOOL_SEND_DETAILS,
        description: "Record a follow-up for the team: details the caller wants sent on WhatsApp, \
             or a request for someone from the team to call them back.",
        parameters: json!({
            "type": "object",
            "properties": { "topic": { "type": "string", "description": "What to send, in English" } },
            "required": ["topic"]
        }),
    });
    tools.push(ToolSpec {
        name: TOOL_SWITCH_LANGUAGE,
        description: "Switch the call to another language: your voice and replies change to it.",
        parameters: json!({
            "type": "object",
            "properties": { "language": { "type": "string", "enum": SWITCHABLE_LANGUAGES } },
            "required": ["language"]
        }),
    });
    tools.push(ToolSpec {
        name: TOOL_END_CALL,
        description: "Hang up the call. Say a short goodbye first.",
        parameters: json!({
            "type": "object",
            "properties": { "reason": { "type": "string" } },
            "required": ["reason"]
        }),
    });
    tools
}

fn message_chars(m: &ChatMessage) -> usize {
    m.content.as_deref().map_or(0, str::len)
        + m.tool_calls
            .iter()
            .map(|c| c.function.arguments.len() + c.function.name.len())
            .sum::<usize>()
}

/// Returns `system` followed by the newest history that fits the budget.
/// Trimming starts at a user message so tool results are never orphaned.
pub fn assemble(system: &str, history: &[ChatMessage]) -> Vec<ChatMessage> {
    let mut start = 0;
    let mut total: usize = history.iter().map(message_chars).sum();
    while total > HISTORY_CHAR_BUDGET && start < history.len() {
        total -= message_chars(&history[start]);
        start += 1;
        while start < history.len() && history[start].role != Role::User {
            total -= message_chars(&history[start]);
            start += 1;
        }
    }
    let mut messages = Vec::with_capacity(history.len() - start + 1);
    messages.push(ChatMessage::text(Role::System, system));
    messages.extend_from_slice(&history[start..]);
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_come_first_and_language_line_is_appended() {
        let parts = SystemPromptParts {
            speaking_rules: "SPEAKING RULES: be brief.",
            system_prompt: "You sell clinic marketing.",
            lead_name: "Ravi",
            kb_context: Some("- Ads from ₹10,000/month"),
            has_kb: true,
            has_booking: false,
            today: "",
            language: None,
            agent_name: None,
        };
        let a = system_message(&parts);
        assert!(a.starts_with("SPEAKING RULES"));
        assert!(a.contains("Ravi"));
        assert!(a.contains("₹10,000"));
        assert!(a.contains("search_knowledge"));
        let b = system_message(&SystemPromptParts {
            language: Some("ta-IN"),
            ..parts
        });
        // Only the language line is inserted; everything before it is unchanged.
        let shared = a.trim_end_matches(REPLY_CHECKLIST);
        assert!(b.starts_with(shared));
        assert!(b.ends_with(REPLY_CHECKLIST));
        assert!(b.contains("Tamil"));
    }

    #[test]
    fn tools_omit_search_without_kb() {
        let names: Vec<_> = tools(false, false).iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            vec![TOOL_SEND_DETAILS, TOOL_SWITCH_LANGUAGE, TOOL_END_CALL]
        );
        assert_eq!(tools(true, false).len(), 4);
        assert_eq!(tools(true, true).len(), 6);
    }

    #[test]
    fn trimming_keeps_newest_and_starts_at_user() {
        let big = "x".repeat(25_000);
        let history = vec![
            ChatMessage::text(Role::User, big.clone()),
            ChatMessage::text(Role::Assistant, big.clone()),
            ChatMessage::text(Role::User, big.clone()),
            ChatMessage::text(Role::Assistant, "short"),
            ChatMessage::text(Role::User, "latest"),
        ];
        let out = assemble("sys", &history);
        assert_eq!(out[0].role, Role::System);
        assert_eq!(out[1].role, Role::User);
        assert_eq!(
            out.last().and_then(|m| m.content.as_deref()),
            Some("latest")
        );
        assert!(out.len() < history.len() + 1);
    }
}
