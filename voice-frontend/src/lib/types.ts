/** Shapes returned by the voice backend's /v1 API. */

export type CallStatus = "queued" | "ringing" | "in_progress" | "completed" | "failed" | "no_answer" | "busy";

export interface CallSummary {
  call_id: string;
  status: CallStatus;
  to: string;
  lead_name: string;
  agent_id: string | null;
  agent_name: string | null;
  language: string;
  detected_language: string | null;
  created_at: string;
  answered_at: string | null;
  ended_at: string | null;
  duration_secs: number | null;
  end_reason: string | null;
  in_progress: boolean;
  interest_level: string | null;
  summary: string | null;
  appointments: number;
  voice_to_voice_p50_ms: number | null;
  review_score: number | null;
  rating: Rating | null;
  cost_excl_gst: number;
  cost_incl_gst: number;
  telephony_pending: boolean;
}

export interface CallList {
  total: number;
  limit: number;
  offset: number;
  calls: CallSummary[];
}

export interface Percentiles {
  p50: number | null;
  p95: number | null;
  n?: number;
}

export interface TurnLatency {
  turn_seq: number;
  speech_end_ms: number | null;
  stt_final_ms: number | null;
  llm_first_token_ms: number | null;
  tts_first_byte_ms: number | null;
  first_audio_sent_ms: number | null;
  spoken_chars: number;
  cap_triggered: boolean;
  llm_model: string | null;
  llm_attempts: number;
}

export interface CallDetail {
  call_id: string;
  status: CallStatus;
  to: string;
  lead_name: string;
  agent_id: string | null;
  agent_name: string | null;
  collect_fields: LeadField[];
  knowledge_base_id: string | null;
  language: string;
  detected_language: string | null;
  voice: string | null;
  created_at: string;
  started_at: string | null;
  answered_at: string | null;
  ended_at: string | null;
  duration_secs: number | null;
  end_reason: string | null;
  in_progress: boolean;
  transcript: {
    seq: number;
    role: "user" | "assistant" | "tool";
    text: string;
    language: string | null;
    interrupted: boolean;
    live_text: string | null;
    at: string;
  }[];
  extracted_fields: ExtractedFields | null;
  follow_ups: { topic: string; at: string }[];
  appointments: { starts_at: string; duration_mins: number; name: string; status: string }[];
  recording_url: string | null;
  review: CallReview | null;
  /** Lessons the reviewer suggested from this call. */
  lessons: Lesson[];
  rating: Rating | null;
  rating_note: string | null;
  latency: {
    turns: number;
    voice_to_voice_ms: Percentiles;
    stt_final_ms: Percentiles;
    llm_first_token_ms: Percentiles;
    tts_first_byte_ms: Percentiles;
    per_turn: TurnLatency[];
  };
  usage: {
    stt_seconds: number;
    tts_chars: number;
    llm: Record<string, { prompt_tokens?: number; cached_prompt_tokens?: number; completion_tokens?: number }>;
  };
  cost: {
    ai: {
      stt: number;
      tts: number;
      llm: number;
      /** Part of `llm`: replies cut short whose usage was estimated. */
      llm_estimated: number;
      /** Greeting audio synthesized for this call. */
      tts_greeting: number;
      extraction: number;
      review: number;
      stt_retranscribe: number;
      total: number;
      tts_cached_chars: number;
    };
    telephony: {
      call: number | null;
      billed_secs: number | null;
      rate_per_min: number | null;
      pending: boolean;
      unused_paid_secs: number | null;
    };
    per_minute_excl_gst: number | null;
    total_excl_gst: number;
    gst_percent: number;
    total_incl_gst: number;
  };
}

/** A person's verdict on a call: 1 good, -1 bad. */
export type Rating = 1 | -1;

export interface CallReview {
  /** 1-10. */
  score: number;
  went_well: string[];
  mistakes: { turn: number | null; kind: string; what: string }[];
  model?: string;
  reviewed_at?: string;
  with_feedback?: boolean;
}

export interface Lesson {
  id: string;
  agent_id: string;
  call_id: string | null;
  /** The instruction the agent will follow. */
  lesson: string;
  /** What went wrong on the call. */
  mistake: string;
  source: "review" | "feedback";
  status: "suggested" | "applied" | "rejected";
  created_at: string;
  decided_at: string | null;
}

export interface Training {
  /** The agent's train.md, added to its prompt on every call. */
  training_notes: string;
  auto_apply_lessons: boolean;
  max_chars: number;
  /** Lessons shared by every agent (prompts/train.md on the server). */
  shared_notes: string;
}

export interface ExtractedFields {
  name: string | null;
  interest_level: "high" | "medium" | "low" | "none" | null;
  budget: string | null;
  preferred_language: string | null;
  follow_up_date: string | null;
  objections: string[];
  summary: string;
  custom?: Record<string, string | number | boolean | null>;
}

export interface Keyed {
  key: string;
  calls: number;
}

export interface Stats {
  from: string;
  to: string;
  days: number;
  gst_percent: number;
  totals: {
    calls: number;
    answered: number;
    answer_rate: number | null;
    talk_minutes: number;
    avg_call_secs: number | null;
    appointments_booked: number;
    active_calls: number;
    stt_seconds: number;
    tts_chars: number;
    reviewed_calls: number;
    avg_review_score: number | null;
  };
  cost: {
    ai: { stt: number; tts: number; llm: number; extraction: number; review: number; stt_retranscribe: number; tts_greeting: number; total: number };
    /** Spend that belongs to no single call. */
    other: { tts_cache: number; prompt_edit: number; total: number };
    telephony: number;
    telephony_pending_calls: number;
    number_rental_per_month: number;
    total_excl_gst: number;
    total_incl_gst: number;
    per_talk_minute_excl_gst: number | null;
    per_answered_call_excl_gst: number | null;
    per_answered_call_incl_gst: number | null;
  };
  latency_ms: {
    turns: number;
    voice_to_voice: Percentiles;
    stt_final: Percentiles;
    llm_first_token: Percentiles;
    tts_first_byte: Percentiles;
  };
  daily: {
    date: string;
    calls: number;
    answered: number;
    talk_minutes: number;
    ai_cost: number;
    telephony_cost: number;
    cost_incl_gst: number;
  }[];
  by_agent: {
    agent_id: string | null;
    name: string;
    calls: number;
    answered: number;
    talk_minutes: number;
    cost_excl_gst: number;
    cost_incl_gst: number;
    appointments: number;
    avg_review_score: number | null;
  }[];
  by_status: Keyed[];
  by_language: Keyed[];
  by_end_reason: Keyed[];
  by_interest: Keyed[];
}

export type FieldKind = "text" | "number" | "date" | "boolean" | "choice";

export interface LeadField {
  key: string;
  label: string;
  description: string;
  type: FieldKind;
  options?: string[];
  required: boolean;
}

export interface Agent {
  id: string;
  name: string;
  description: string;
  prompt: string;
  greeting: string | null;
  knowledge_base_id: string | null;
  language: string;
  voice: string | null;
  booking: boolean;
  max_duration_secs: number | null;
  fields: LeadField[];
  created_at: string;
  updated_at: string;
  calls?: number;
  last_call_at?: string | null;
  avg_review_score?: number | null;
  /** Lessons waiting for approval. */
  suggested_lessons?: number;
}

export interface KnowledgeBase {
  id: string;
  name: string;
  created_at: string;
  documents: number;
  ready_documents: number;
  chunks: number;
  size_bytes: number;
}

export interface KbDocument {
  id: string;
  filename: string;
  status: "pending" | "processing" | "ready" | "failed";
  error: string | null;
  size_bytes: number | null;
  chunks: number;
  created_at: string;
}

export interface KbDocumentDetail extends Omit<KbDocument, "chunks"> {
  content_type: string | null;
  /** The uploaded text, for Markdown and plain-text files up to 1 MB. */
  text: string | null;
  /** Signed link to the uploaded file; lasts `file_url_expires_in_secs`. */
  file_url: string | null;
  file_url_expires_in_secs: number;
  chunks: { seq: number; content: string; tokens: number }[];
}

export interface SearchHit {
  content: string;
  score: number;
  document_id?: string;
  filename?: string;
  seq?: number;
}

export interface Options {
  personas: { name: string; description: string; prompt: string }[];
  languages: { code: string; speaker: string; model: string; agent_name: string | null }[];
  fallback_language: string;
  llm_models: string[];
  default_llm_model: string;
  tts_models: string[];
  default_tts_model: string;
  default_max_duration_secs: number;
  greeting_max_chars: number;
  call_hours: { enforced: boolean; start_hour: number; end_hour: number; timezone: string; open_now: boolean };
  embedding_model_ready: boolean;
}
