//! Agents: a reusable character for calls. An agent holds the character
//! prompt, the lead details the call should collect (which post-call
//! extraction then fills), a knowledge base and per-call defaults.

use chrono::{DateTime, Utc};
use common::{
    ids::{AgentId, KbId, TenantId},
    AppError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{calls::check_greeting, util::is_language};
use crate::state::AppState;

/// Most fields one agent may collect.
pub const MAX_FIELDS: usize = 20;

/// The kind of value a lead field holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    /// Free text.
    #[default]
    Text,
    /// A number (budget, count, age).
    Number,
    /// A date, stored as YYYY-MM-DD.
    Date,
    /// Yes or no.
    Boolean,
    /// One of `options`.
    Choice,
}

/// One lead detail the agent asks for during the call and extraction
/// fills afterwards.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeadField {
    /// Key in the extracted data, `snake_case` (e.g. `city`).
    pub key: String,
    /// Human label (e.g. "City").
    pub label: String,
    /// What to ask or capture, for the agent and the extractor.
    #[serde(default)]
    pub description: String,
    /// Value kind.
    #[serde(default, rename = "type")]
    pub kind: FieldKind,
    /// Allowed answers for `choice`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// The agent should get this before ending the call.
    #[serde(default)]
    pub required: bool,
}

/// An agent as stored and returned by the API.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Agent {
    /// Agent id.
    pub id: uuid::Uuid,
    /// Display name.
    pub name: String,
    /// Short note on what the agent is for.
    pub description: String,
    /// Character prompt: who the agent is and what the call is for.
    pub prompt: String,
    /// Greeting after the fixed AI/recording disclosure.
    pub greeting: Option<String>,
    /// Knowledge base searched during the call.
    #[serde(rename = "knowledge_base_id")]
    pub kb_id: Option<uuid::Uuid>,
    /// `auto` or BCP-47.
    pub language: String,
    /// TTS speaker override.
    pub voice: Option<String>,
    /// Lets the agent check and book appointments.
    pub booking: bool,
    /// Call length cap.
    pub max_duration_secs: Option<i32>,
    /// Lead details to collect.
    #[sqlx(json)]
    pub fields: Vec<LeadField>,
    /// The agent's train.md: lessons learned from past calls, added to its
    /// prompt on every call (edited through `/v1/agents/{id}/training`).
    pub training_notes: String,
    /// Append suggested lessons to the notes without waiting for approval.
    pub auto_apply_lessons: bool,
    /// Created.
    pub created_at: DateTime<Utc>,
    /// Last edited.
    pub updated_at: DateTime<Utc>,
}

/// `POST /v1/agents` and `PUT /v1/agents/{id}` body.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentInput {
    /// Display name, 1-100 characters.
    pub name: String,
    /// Short note on what the agent is for.
    #[serde(default)]
    pub description: String,
    /// Character prompt, 1-20000 bytes.
    pub prompt: String,
    /// Greeting after the disclosure (at most 100 characters).
    #[serde(default)]
    pub greeting: Option<String>,
    /// Knowledge base to search.
    #[serde(default)]
    pub knowledge_base_id: Option<KbId>,
    /// `auto` or BCP-47 (default `auto`).
    #[serde(default)]
    pub language: Option<String>,
    /// TTS speaker override.
    #[serde(default)]
    pub voice: Option<String>,
    /// Appointment booking on or off.
    #[serde(default)]
    pub booking: bool,
    /// Call length cap, 10-3600 s.
    #[serde(default)]
    pub max_duration_secs: Option<u32>,
    /// Lead details to collect.
    #[serde(default)]
    pub fields: Vec<LeadField>,
}

fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && key.len() <= 40
}

/// Checks an agent body and trims its text; the knowledge base must belong
/// to the tenant.
pub async fn validate(
    state: &AppState,
    tenant: TenantId,
    mut input: AgentInput,
) -> Result<AgentInput, AppError> {
    input.name = input.name.trim().to_owned();
    input.description = input.description.trim().to_owned();
    input.prompt = input.prompt.trim().to_owned();
    if input.name.is_empty() || input.name.chars().count() > 100 {
        return Err(AppError::bad_request("`name` must be 1-100 characters"));
    }
    if input.description.chars().count() > 500 {
        return Err(AppError::bad_request(
            "`description` must be at most 500 characters",
        ));
    }
    if input.prompt.is_empty() || input.prompt.len() > 20_000 {
        return Err(AppError::bad_request("`prompt` must be 1-20000 bytes"));
    }
    input.greeting = input
        .greeting
        .map(|g| g.trim().to_owned())
        .filter(|g| !g.is_empty());
    if let Some(g) = &input.greeting {
        check_greeting(g)?;
    }
    input.voice = input
        .voice
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    let language = input.language.as_deref().map(str::trim).unwrap_or("auto");
    if !is_language(language) {
        return Err(AppError::bad_request(
            "`language` must be `auto` or a BCP-47 code like hi-IN",
        ));
    }
    input.language = Some(language.to_owned());
    if input
        .max_duration_secs
        .is_some_and(|d| !(10..=3600).contains(&d))
    {
        return Err(AppError::bad_request(
            "`max_duration_secs` must be between 10 and 3600",
        ));
    }
    if input.booking && state.booking.is_none() {
        return Err(AppError::bad_request(
            "`booking` needs a valid config/booking.toml",
        ));
    }
    if input.fields.len() > MAX_FIELDS {
        return Err(AppError::bad_request(format!(
            "at most {MAX_FIELDS} `fields`"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for f in &mut input.fields {
        f.key = f.key.trim().to_owned();
        f.label = f.label.trim().to_owned();
        f.description = f.description.trim().to_owned();
        f.options = f
            .options
            .iter()
            .map(|o| o.trim().to_owned())
            .filter(|o| !o.is_empty())
            .collect();
        if !valid_key(&f.key) {
            return Err(AppError::bad_request(format!(
                "field key `{}` must be snake_case (a-z, 0-9, _), starting with a letter, at most 40 characters",
                f.key
            )));
        }
        if !seen.insert(f.key.clone()) {
            return Err(AppError::bad_request(format!(
                "field key `{}` is repeated",
                f.key
            )));
        }
        if f.label.is_empty() || f.label.chars().count() > 80 {
            return Err(AppError::bad_request(format!(
                "field `{}` needs a label of 1-80 characters",
                f.key
            )));
        }
        if f.description.chars().count() > 300 {
            return Err(AppError::bad_request(format!(
                "field `{}` description must be at most 300 characters",
                f.key
            )));
        }
        match f.kind {
            FieldKind::Choice if f.options.len() < 2 || f.options.len() > 20 => {
                return Err(AppError::bad_request(format!(
                    "choice field `{}` needs 2-20 options",
                    f.key
                )));
            }
            FieldKind::Choice => {}
            _ => f.options.clear(),
        }
    }
    if let Some(kb) = input.knowledge_base_id {
        let exists: Option<(uuid::Uuid,)> =
            sqlx::query_as("SELECT id FROM knowledge_bases WHERE id = $1 AND tenant_id = $2")
                .bind(kb.0)
                .bind(tenant.0)
                .fetch_optional(&state.db)
                .await?;
        if exists.is_none() {
            return Err(AppError::not_found("knowledge base not found"));
        }
    }
    Ok(input)
}

/// Loads one agent of the tenant.
pub async fn get(state: &AppState, tenant: TenantId, id: AgentId) -> Result<Agent, AppError> {
    sqlx::query_as("SELECT * FROM agents WHERE id = $1 AND tenant_id = $2")
        .bind(id.0)
        .bind(tenant.0)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::not_found("agent not found"))
}

/// The system-prompt section that tells the agent which details to collect.
/// Empty when there are no fields.
pub fn collect_prompt(fields: &[LeadField]) -> String {
    if fields.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "DETAILS TO COLLECT: during the call, find out the details below. Ask one at a time, \
         only where it fits the conversation, and skip any the caller already gave. Get the \
         required ones before you end the call; never insist on the others. The notes below \
         are for you only: ask in plain words and never read out formats or units.",
    );
    for f in fields {
        out.push_str(&format!("\n- {}", f.label));
        if f.required {
            out.push_str(" (required)");
        }
        if !f.description.is_empty() {
            out.push_str(&format!(": {}", f.description));
        }
        if !f.options.is_empty() {
            out.push_str(&format!(" Possible answers: {}.", f.options.join(", ")));
        }
    }
    out
}

/// JSON schema of the `custom` object extraction returns for these fields.
pub fn custom_schema(fields: &[LeadField]) -> Value {
    let props: serde_json::Map<String, Value> = fields
        .iter()
        .map(|f| {
            let schema = match f.kind {
                FieldKind::Text => json!({ "type": ["string", "null"] }),
                FieldKind::Number => json!({ "type": ["number", "null"] }),
                FieldKind::Date => {
                    json!({ "type": ["string", "null"], "description": "YYYY-MM-DD" })
                }
                FieldKind::Boolean => json!({ "type": ["boolean", "null"] }),
                FieldKind::Choice => {
                    let mut options: Vec<Value> =
                        f.options.iter().map(|o| Value::String(o.clone())).collect();
                    options.push(Value::Null);
                    json!({ "type": ["string", "null"], "enum": options })
                }
            };
            (f.key.clone(), schema)
        })
        .collect();
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": fields.iter().map(|f| f.key.clone()).collect::<Vec<_>>(),
        "properties": props,
    })
}

/// How the extractor is told about the custom fields.
pub fn custom_instructions(fields: &[LeadField]) -> String {
    let mut out = String::from(
        " Also return `custom`, an object with exactly these keys. Fill a key from what the caller \
         said (an approximate answer such as \"around 50 lakh\" counts); use null only when the \
         caller never mentioned it, and never guess:",
    );
    for f in fields {
        let kind = match f.kind {
            FieldKind::Text => "text".to_owned(),
            FieldKind::Number => "number".to_owned(),
            FieldKind::Date => "date YYYY-MM-DD".to_owned(),
            FieldKind::Boolean => "true or false".to_owned(),
            FieldKind::Choice => format!("one of: {}", f.options.join(", ")),
        };
        out.push_str(&format!("\n- {} ({}; {kind})", f.key, f.label));
        if !f.description.is_empty() {
            out.push_str(&format!(": {}", f.description));
        }
    }
    out
}

/// Keeps only the declared keys of an extracted `custom` object, with values
/// of the declared kind (anything else becomes null).
pub fn clean_custom(fields: &[LeadField], raw: Option<&Value>) -> serde_json::Map<String, Value> {
    fields
        .iter()
        .map(|f| {
            let v = raw
                .and_then(|r| r.get(&f.key))
                .cloned()
                .unwrap_or(Value::Null);
            let v = match (f.kind, &v) {
                (FieldKind::Number, Value::Number(_)) => v,
                (FieldKind::Number, Value::String(s)) => s
                    .replace(',', "")
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map_or(Value::Null, Value::Number),
                (FieldKind::Boolean, Value::Bool(_)) => v,
                (FieldKind::Choice, Value::String(s)) => f
                    .options
                    .iter()
                    .find(|o| o.eq_ignore_ascii_case(s.trim()))
                    .map_or(Value::Null, |o| Value::String(o.clone())),
                (FieldKind::Text | FieldKind::Date, Value::String(s)) if !s.trim().is_empty() => {
                    Value::String(s.trim().to_owned())
                }
                _ => Value::Null,
            };
            (f.key.clone(), v)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn field(key: &str, kind: FieldKind, options: &[&str]) -> LeadField {
        LeadField {
            key: key.into(),
            label: key.to_uppercase(),
            description: String::new(),
            kind,
            options: options.iter().map(|s| (*s).into()).collect(),
            required: key == "city",
        }
    }

    #[test]
    fn keys_are_snake_case() {
        assert!(valid_key("city"));
        assert!(valid_key("move_in_date2"));
        assert!(!valid_key("City"));
        assert!(!valid_key("2city"));
        assert!(!valid_key("my-city"));
        assert!(!valid_key(""));
    }

    #[test]
    fn prompt_lists_the_fields() {
        assert_eq!(collect_prompt(&[]), "");
        let p = collect_prompt(&[
            field("city", FieldKind::Text, &[]),
            field("plan", FieldKind::Choice, &["basic", "pro"]),
        ]);
        assert!(p.contains("- CITY (required)"));
        assert!(p.contains("Possible answers: basic, pro."));
    }

    #[test]
    fn extracted_values_are_coerced_to_their_kind() {
        let fields = [
            field("city", FieldKind::Text, &[]),
            field("budget", FieldKind::Number, &[]),
            field("plan", FieldKind::Choice, &["Basic", "Pro"]),
            field("owner", FieldKind::Boolean, &[]),
        ];
        let raw = json!({
            "city": " Mysuru ", "budget": "25,000", "plan": "pro", "owner": "yes", "extra": 1
        });
        let out = clean_custom(&fields, Some(&raw));
        assert_eq!(out["city"], "Mysuru");
        assert_eq!(out["budget"], 25000.0);
        assert_eq!(out["plan"], "Pro");
        assert_eq!(out["owner"], Value::Null);
        assert!(!out.contains_key("extra"));
        let schema = custom_schema(&fields);
        assert_eq!(schema["required"].as_array().map(Vec::len), Some(4));
        assert_eq!(
            schema["properties"]["plan"]["enum"],
            json!(["Basic", "Pro", null])
        );
    }
}
