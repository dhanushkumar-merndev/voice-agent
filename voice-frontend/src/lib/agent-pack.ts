/**
 * Agent packs: a ZIP holding one agent and its knowledge, so anyone can
 * build an agent by pasting PACK_PROMPT into ChatGPT or Claude with their
 * business files and importing the ZIP it returns.
 *
 *   agent.json          the agent (same fields as POST /v1/agents)
 *   knowledge/*.md      knowledge-base files (.md, .txt or .pdf)
 *   train.md            optional training notes
 *
 * Everything here runs in the browser before anything is sent. The backend
 * checks every value again, fences the text inside the agent's prompt and
 * keeps its own safety rules first, so a pack can describe an agent but
 * can never switch off the AI/recording notice, calling hours or rules.
 *
 * No `@/` imports, so `node` can load this file directly in tests.
 */

import { strFromU8, strToU8, unzipSync, zipSync } from "fflate";

/** Pack format version written by exports. */
export const PACK_VERSION = 1;
/** Largest ZIP accepted (bytes). */
export const MAX_PACK_BYTES = 60 * 1024 * 1024;
/** Largest total unpacked size (bytes): stops ZIP bombs. */
export const MAX_UNPACKED_BYTES = 80 * 1024 * 1024;
/** Largest knowledge file, as on the server (bytes). */
export const MAX_FILE_BYTES = 20 * 1024 * 1024;
/** Most knowledge files in one pack. */
export const MAX_KNOWLEDGE_FILES = 50;
/** Most entries of any kind in the ZIP. */
const MAX_ENTRIES = 200;
/** Longest training notes (characters), as on the server. */
export const MAX_NOTES_CHARS = 8000;
/** Longest greeting (characters), as on the server. */
export const MAX_GREETING_CHARS = 100;
const MAX_FIELDS = 20;
const FIELD_TYPES = ["text", "number", "date", "boolean", "choice"] as const;
const KNOWLEDGE_TYPES: Record<string, string> = { md: "text/markdown", txt: "text/plain", pdf: "application/pdf" };
const AGENT_KEYS = ["name", "description", "prompt", "greeting", "language", "booking", "max_duration_secs", "fields"];
/** Keys an export from another install may carry; dropped on import. */
const DROPPED_KEYS = ["pack_version", "id", "knowledge_base_id", "voice", "created_at", "updated_at"];
const FIELD_KEYS = ["key", "label", "description", "type", "options", "required"];

/**
 * Phrases that try to take over the agent: override its rules, change its
 * role, hide the AI/recording notice or smuggle chat-template tokens. Text
 * in a pack containing one is refused, wherever it is.
 */
const TAMPER_PATTERNS: { re: RegExp; why: string }[] = [
  { re: /\b(ignore|disregard|forget|override|bypass)\b[^.\n]{0,40}\b(previous|prior|above|earlier|all|any|your|the|these|system|safety)\b[^.\n]{0,20}\b(instructions?|rules?|prompts?|guidelines?|polic(y|ies))\b/i, why: "tells the agent to ignore its rules" },
  { re: /\b(reveal|print|show|repeat|share|output)\b[^.\n]{0,30}\b(system prompt|your (instructions|prompt|rules))\b/i, why: "asks the agent to reveal its instructions" },
  { re: /\b(developer|god|jailbreak|dan|unrestricted)\s+mode\b/i, why: "tries to unlock a special mode" },
  { re: /\byou are (now|no longer)\b/i, why: "tries to change who the agent is" },
  { re: /\b(do not|don't|never)\s+(say|mention|tell|disclose|admit|reveal)\b[^.\n]{0,40}\b(ai|a\.i\.|bot|robot|recorded|recording|artificial)\b/i, why: "hides that the caller is talking to an AI or being recorded" },
  { re: /\b(pretend|claim|say|insist)\b[^.\n]{0,20}\b(to be|you are|you're|that you are)\s+(a\s+)?(human|real person|not an? ai|not a bot)\b/i, why: "makes the agent claim to be human" },
  { re: /\b(this call is not|calls are not|not being) recorded\b/i, why: "says the call is not recorded" },
  { re: /<\|[a-z_]*\|>|\[\/?INST\]|<<\/?SYS>>|<\/?(persona|knowledge)>/i, why: "contains hidden control tags" },
  // Case-sensitive: an ordinary "## Security" heading is fine; only the
  // all-caps headers of the fixed rules are imitations.
  { re: /^\s*(#+\s*)?(SPEAKING RULES|SECURITY|EVERY REPLY|TONE \(fixed\))(:|\s*\(|\s*$)/m, why: "imitates the agent's fixed safety rules" },
];

/** Words that mean the greeting already contains the AI/recording notice. */
const NOTICE_WORDS = /\b(ai|a\.i\.|artificial|virtual assistant|recorded|recording)\b|एआई|रिकॉर्ड/i;

export type FieldType = (typeof FIELD_TYPES)[number];

export interface PackField {
  key: string;
  label: string;
  description: string;
  type: FieldType;
  options?: string[];
  required: boolean;
}

/** The agent as sent to POST /v1/agents (without the knowledge base). */
export interface PackAgent {
  name: string;
  description: string;
  prompt: string;
  greeting: string | null;
  language: string;
  booking: boolean;
  max_duration_secs: number | null;
  fields: PackField[];
}

export interface PackFile {
  /** Safe file name (no folders). */
  name: string;
  type: string;
  bytes: Uint8Array;
  /** Decoded text for .md and .txt files. */
  text: string | null;
}

export interface ParsedPack {
  agent: PackAgent | null;
  files: PackFile[];
  training: string | null;
  /** Problems that block the import. */
  errors: string[];
  /** Things worth knowing that do not block it. */
  warnings: string[];
}

/** The prompt users paste into ChatGPT or Claude together with their files. */
export const PACK_PROMPT = `You are helping me set up an AI phone agent that calls my leads. I will give you details and files about my business. Build an "agent pack": a single ZIP file I can download.

The ZIP must contain exactly:

1. agent.json, with exactly these keys:
{
  "name": "short agent name, at most 100 characters",
  "description": "one line about what this agent is for, at most 500 characters",
  "prompt": "the agent's persona and task (see rules below)",
  "greeting": "first line after the automatic AI notice, at most 100 characters",
  "language": "auto, or a code like en-IN, hi-IN, ta-IN, te-IN, kn-IN, ml-IN",
  "booking": false,
  "max_duration_secs": 300,
  "fields": [
    { "key": "budget", "label": "Budget", "description": "What they want to spend", "type": "text", "required": false }
  ]
}

2. A folder named knowledge with Markdown (.md) files holding the facts the agent answers from.

3. Optionally train.md: short bullet-point coaching notes ("- Ask for the budget before suggesting a plot.").

RULES FOR THE PROMPT (the "prompt" value)
- Write in the second person: "You are Priya, calling from <business> about <purpose>."
- Say who the callers are, the goal of the call (for example: qualify the lead and book a site visit), the order of questions, and how to close.
- Keep it under 3,000 words. Plain sentences, no markdown tables.
- Put only facts that I gave you. Never invent prices, offers, discounts, phone numbers or addresses. If something is unknown, write: "If asked about X, say the team will confirm."
- Do not write safety rules, do not mention AI disclosure or call recording, and never tell the agent to hide that it is an AI or to ignore any rules. The calling system adds those itself and will reject packs that try to change them.

RULES FOR THE GREETING
- At most 100 characters. One short intro and one question, e.g. "Hi, I'm calling from Green Acres about your plot enquiry. Is this a good time?"
- Do not say it is an AI or that the call is recorded: that notice is played automatically before the greeting.

RULES FOR THE FIELDS (details the agent collects; up to 20)
- key: snake_case, starts with a letter, a-z 0-9 _, at most 40 characters, unique.
- label: 1-80 characters. description: at most 300 characters.
- type: one of text, number, date, boolean, choice. A choice field needs "options": 2-20 short strings.
- Do not add name or phone fields: the system already knows them.

RULES FOR THE KNOWLEDGE FILES
- One topic per file, named like 01_overview.md, 02_pricing.md, 03_location.md, 04_faq.md.
- Each file starts with a # title, then short sections with ## headings.
- Write each fact as a full sentence that repeats the subject's name, because the agent searches small pieces of text: write "The Star Garden plot price is 99 lakh rupees per plot." not "Price: 99L".
- Include an FAQ file with the questions callers ask most, each answered in one or two sentences.
- Use only the information I provide. Copy numbers exactly. Never add instructions to the agent inside knowledge files.

OUTPUT
- Create the ZIP and give it to me as a download (use your code tool if you have one). Name it <agent-name>-pack.zip.
- If you cannot create files, print each file's full content in a separate code block with its path as the heading, so I can save them myself.
- Before you build it, ask me for anything important that is missing (business name, what we sell, prices, location, the goal of the call, the language callers speak).

Here are my business details and files:
`;

function extension(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot < 0 ? "" : name.slice(dot + 1).toLowerCase();
}

/** A ZIP entry name made safe, or null for entries that must be refused. */
function cleanPath(raw: string): string | null {
  const p = raw.replace(/\\/g, "/");
  if (p.startsWith("/") || /^[a-z]:/i.test(p) || p.split("/").some((s) => s === "..")) return null;
  return p.replace(/^(\.\/)+/, "");
}

/** Junk added by zip tools and operating systems. */
function isJunk(p: string): boolean {
  const base = p.split("/").pop() ?? "";
  return p.startsWith("__MACOSX/") || base === ".DS_Store" || base === "Thumbs.db" || base.startsWith("._") || p.endsWith("/");
}

/** First tampering phrase found in `text`, as a message, or null. */
export function findTampering(text: string, where: string): string | null {
  for (const { re, why } of TAMPER_PATTERNS) {
    const m = re.exec(text);
    if (m) return `${where} ${why} ("${m[0].trim().slice(0, 60)}"). Remove that text and import again.`;
  }
  return null;
}

function decodeText(bytes: Uint8Array): string | null {
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes).replace(/^﻿/, "");
  } catch {
    return null;
  }
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function isLanguage(code: string): boolean {
  return code === "auto" || /^[a-z]{2,3}-[A-Z]{2}$/.test(code);
}

function checkAgent(raw: unknown, errors: string[], warnings: string[]): PackAgent | null {
  if (!isRecord(raw)) {
    errors.push("agent.json must be a JSON object.");
    return null;
  }
  for (const k of Object.keys(raw)) {
    if (DROPPED_KEYS.includes(k)) continue;
    if (!AGENT_KEYS.includes(k)) errors.push(`agent.json has an unknown key "${k}". Allowed: ${AGENT_KEYS.join(", ")}.`);
  }
  const str = (k: string): string | null => {
    const v = raw[k];
    if (v === undefined || v === null) return null;
    if (typeof v !== "string") {
      errors.push(`agent.json "${k}" must be text.`);
      return null;
    }
    return v.trim();
  };
  const name = str("name") ?? "";
  const description = str("description") ?? "";
  const prompt = str("prompt") ?? "";
  const greeting = str("greeting") || null;
  const language = str("language") || "auto";

  if (!name || [...name].length > 100) errors.push("The agent needs a name of 1-100 characters.");
  if ([...description].length > 500) errors.push("The description must be at most 500 characters.");
  const promptBytes = new TextEncoder().encode(prompt).length;
  if (!prompt || promptBytes > 20000) errors.push(`The prompt must be 1-20,000 bytes (it is ${promptBytes}).`);
  if (greeting) {
    const n = [...greeting].length;
    if (n > MAX_GREETING_CHARS) errors.push(`The greeting is ${n} characters; keep it at most ${MAX_GREETING_CHARS}.`);
    if (NOTICE_WORDS.test(greeting)) {
      warnings.push("The greeting mentions AI or recording. That notice is already played first, so it may be said twice.");
    }
  }
  if (!isLanguage(language)) errors.push(`Language "${language}" must be "auto" or a code like en-IN or hi-IN.`);

  let booking = false;
  if (raw.booking !== undefined && raw.booking !== null) {
    if (typeof raw.booking !== "boolean") errors.push('agent.json "booking" must be true or false.');
    else booking = raw.booking;
  }
  if (booking) warnings.push("Booking is on. It works only if the server has a booking calendar set up.");

  let maxDuration: number | null = null;
  if (raw.max_duration_secs !== undefined && raw.max_duration_secs !== null) {
    const d = raw.max_duration_secs;
    if (typeof d !== "number" || !Number.isInteger(d) || d < 10 || d > 3600) {
      errors.push("max_duration_secs must be a whole number from 10 to 3600.");
    } else maxDuration = d;
  }

  const fields: PackField[] = [];
  const rawFields = raw.fields ?? [];
  if (!Array.isArray(rawFields)) errors.push('agent.json "fields" must be a list.');
  else {
    if (rawFields.length > MAX_FIELDS) errors.push(`At most ${MAX_FIELDS} fields (found ${rawFields.length}).`);
    const seen = new Set<string>();
    rawFields.slice(0, MAX_FIELDS).forEach((f, i) => {
      const at = `Field ${i + 1}`;
      if (!isRecord(f)) {
        errors.push(`${at} must be an object.`);
        return;
      }
      for (const k of Object.keys(f)) if (!FIELD_KEYS.includes(k)) errors.push(`${at} has an unknown key "${k}".`);
      const key = typeof f.key === "string" ? f.key.trim() : "";
      const label = typeof f.label === "string" ? f.label.trim() : "";
      const desc = typeof f.description === "string" ? f.description.trim() : "";
      const type = (typeof f.type === "string" ? f.type.trim() : "text") as FieldType;
      if (!/^[a-z][a-z0-9_]{0,39}$/.test(key)) errors.push(`${at}: key "${key}" must be snake_case (a-z, 0-9, _), start with a letter, at most 40 characters.`);
      else if (seen.has(key)) errors.push(`${at}: key "${key}" is repeated.`);
      seen.add(key);
      if (!label || [...label].length > 80) errors.push(`${at} (${key}) needs a label of 1-80 characters.`);
      if ([...desc].length > 300) errors.push(`${at} (${key}): description must be at most 300 characters.`);
      if (!FIELD_TYPES.includes(type)) errors.push(`${at} (${key}): type must be one of ${FIELD_TYPES.join(", ")}.`);
      let options: string[] | undefined;
      if (type === "choice") {
        const o = Array.isArray(f.options) ? f.options.filter((x): x is string => typeof x === "string").map((x) => x.trim()).filter(Boolean) : [];
        if (o.length < 2 || o.length > 20) errors.push(`${at} (${key}): a choice field needs 2-20 options.`);
        options = o;
      }
      if (f.required !== undefined && typeof f.required !== "boolean") errors.push(`${at} (${key}): "required" must be true or false.`);
      fields.push({ key, label, description: desc, type, ...(options ? { options } : {}), required: f.required === true });
    });
  }

  for (const [where, text] of [
    ["The prompt", prompt],
    ["The greeting", greeting ?? ""],
    ["The description", description],
    ...fields.flatMap((f) => [[`Field ${f.key}`, `${f.label}\n${f.description}\n${(f.options ?? []).join("\n")}`]]),
  ] as const) {
    const t = findTampering(text, where);
    if (t) errors.push(t);
  }

  return { name, description, prompt, greeting, language, booking, max_duration_secs: maxDuration, fields };
}

/** Unzips and checks a pack. Never throws; problems are in `errors`. */
export function parsePack(zip: Uint8Array): ParsedPack {
  const out: ParsedPack = { agent: null, files: [], training: null, errors: [], warnings: [] };
  if (zip.byteLength > MAX_PACK_BYTES) {
    out.errors.push(`The ZIP is larger than ${MAX_PACK_BYTES / 1024 / 1024} MB.`);
    return out;
  }
  let entries: Record<string, Uint8Array>;
  let total = 0;
  let count = 0;
  let tooBig = false;
  try {
    // The filter sees each entry's declared size before inflating it, so
    // oversized entries are skipped without using any memory.
    entries = unzipSync(zip, {
      filter: (f) => {
        count += 1;
        total += f.originalSize;
        if (f.originalSize > MAX_FILE_BYTES || total > MAX_UNPACKED_BYTES || count > MAX_ENTRIES) {
          tooBig = true;
          return false;
        }
        return true;
      },
    });
  } catch {
    out.errors.push("This is not a valid ZIP file.");
    return out;
  }
  if (tooBig) {
    out.errors.push(`The ZIP is too large: at most ${MAX_ENTRIES} entries, ${MAX_FILE_BYTES / 1024 / 1024} MB per file and ${MAX_UNPACKED_BYTES / 1024 / 1024} MB unpacked.`);
    return out;
  }

  // Clean names, drop junk, refuse paths that leave the ZIP.
  const files = new Map<string, Uint8Array>();
  for (const [raw, bytes] of Object.entries(entries)) {
    const p = cleanPath(raw);
    if (p === null) {
      out.errors.push(`Unsafe path in the ZIP: "${raw}".`);
      continue;
    }
    if (!isJunk(p) && p) files.set(p, bytes);
  }
  // ZIPs made by "compress folder" put everything in one top folder.
  const tops = new Set([...files.keys()].map((p) => (p.includes("/") ? p.split("/")[0] : "")));
  if (!files.has("agent.json") && tops.size === 1 && !tops.has("")) {
    const prefix = `${[...tops][0]}/`;
    const inner = new Map<string, Uint8Array>();
    for (const [p, b] of files) inner.set(p.slice(prefix.length), b);
    files.clear();
    for (const [p, b] of inner) files.set(p, b);
  }

  const agentBytes = files.get("agent.json");
  if (!agentBytes) out.errors.push("agent.json is missing from the ZIP.");
  else {
    const text = decodeText(agentBytes);
    let json: unknown = null;
    try {
      json = text === null ? null : JSON.parse(text);
    } catch (e) {
      out.errors.push(`agent.json is not valid JSON: ${e instanceof Error ? e.message : String(e)}`);
    }
    if (text === null) out.errors.push("agent.json must be UTF-8 text.");
    if (json !== null) out.agent = checkAgent(json, out.errors, out.warnings);
  }

  const train = files.get("train.md");
  if (train) {
    const text = decodeText(train)?.replace(/\r\n/g, "\n").trim() ?? null;
    if (text === null) out.errors.push("train.md must be UTF-8 text.");
    else if ([...text].length > MAX_NOTES_CHARS) out.errors.push(`train.md must be at most ${MAX_NOTES_CHARS} characters.`);
    else {
      const t = findTampering(text, "train.md");
      if (t) out.errors.push(t);
      out.training = text || null;
    }
  }

  const names = new Set<string>();
  for (const [p, bytes] of files) {
    if (p === "agent.json" || p === "train.md") continue;
    if (!p.startsWith("knowledge/")) {
      out.warnings.push(`Skipped "${p}": only agent.json, train.md and files in knowledge/ are used.`);
      continue;
    }
    const name = p.split("/").pop() ?? "";
    const ext = extension(name);
    const type = KNOWLEDGE_TYPES[ext];
    if (!type) {
      out.warnings.push(`Skipped "${p}": knowledge files must be .md, .txt or .pdf.`);
      continue;
    }
    const safe = name.replace(/[^\p{L}\p{N}._ -]/gu, "_").slice(-120);
    if (names.has(safe.toLowerCase())) {
      out.errors.push(`Two knowledge files are named "${safe}". Rename one.`);
      continue;
    }
    names.add(safe.toLowerCase());
    if (bytes.byteLength === 0) {
      out.warnings.push(`Skipped "${p}": the file is empty.`);
      continue;
    }
    let text: string | null = null;
    if (ext === "pdf") {
      if (strFromU8(bytes.subarray(0, 5), true) !== "%PDF-") {
        out.errors.push(`"${p}" is not a real PDF file.`);
        continue;
      }
    } else {
      text = decodeText(bytes);
      if (text === null) {
        out.errors.push(`"${p}" must be UTF-8 text.`);
        continue;
      }
      const t = findTampering(text, `"${p}"`);
      if (t) out.errors.push(t);
    }
    out.files.push({ name: safe, type, bytes, text });
  }
  if (out.files.length > MAX_KNOWLEDGE_FILES) out.errors.push(`At most ${MAX_KNOWLEDGE_FILES} knowledge files (found ${out.files.length}).`);
  if (out.files.length === 0 && out.agent) out.warnings.push("No knowledge files: the agent will answer only from its prompt.");
  out.files.sort((a, b) => a.name.localeCompare(b.name));
  return out;
}

/** Builds a pack ZIP (the reverse of `parsePack`). */
export function buildPack(agent: PackAgent, files: { name: string; bytes: Uint8Array }[], training: string | null): Uint8Array {
  const entries: Record<string, Uint8Array> = {
    "agent.json": strToU8(JSON.stringify({ pack_version: PACK_VERSION, ...agent }, null, 2) + "\n"),
  };
  for (const f of files) entries[`knowledge/${f.name}`] = f.bytes;
  if (training?.trim()) entries["train.md"] = strToU8(training.trim() + "\n");
  return zipSync(entries, { level: 6 });
}

/** A file name for a pack of `agentName`. */
export function packFileName(agentName: string): string {
  const slug = agentName.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 60) || "agent";
  return `${slug}-pack.zip`;
}
