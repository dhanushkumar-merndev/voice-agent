#!/usr/bin/env node
// End-to-end check of the dashboard against the running voice backend:
// sign-in, the API proxy, every page, knowledge-base upload -> embedding ->
// search, agent create/edit/delete, training notes and lessons, and call
// recordings. Leaves no data behind.
//
//   node scripts/e2e.mjs [http://127.0.0.1:3100]
// Reads DASHBOARD_PASSWORD from the environment or the root ../.env.

import { readFileSync } from "node:fs";

const BASE = (process.argv[2] ?? "http://127.0.0.1:3100").replace(/\/$/, "");

function envLocal(name) {
  if (process.env[name]) return process.env[name];
  try {
    const line = readFileSync(new URL("../../.env", import.meta.url), "utf8")
      .split("\n")
      .find((l) => l.startsWith(`${name}=`));
    return line?.slice(name.length + 1).trim();
  } catch {
    return undefined;
  }
}

let passed = 0;
let cookie = "";
function check(ok, what, detail = "") {
  if (!ok) {
    console.error(`FAIL  ${what}${detail ? `: ${detail}` : ""}`);
    process.exit(1);
  }
  passed++;
  console.log(`ok    ${what}`);
}

async function req(method, path, { body, headers = {}, raw } = {}) {
  const h = { ...headers };
  if (cookie) h.cookie = cookie;
  if (body !== undefined && !raw) h["content-type"] = "application/json";
  if (method === "POST" && !h["idempotency-key"]) h["idempotency-key"] = crypto.randomUUID();
  const res = await fetch(BASE + path, {
    method,
    headers: h,
    body: raw ?? (body === undefined ? undefined : JSON.stringify(body)),
    redirect: "manual",
  });
  const text = await res.text();
  let json;
  try {
    json = JSON.parse(text);
  } catch {}
  return { status: res.status, json, text, headers: res.headers };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// --- sign-in -----------------------------------------------------------------
const password = envLocal("DASHBOARD_PASSWORD");
let r = await req("GET", "/");
const open = r.status === 200;
if (!open) {
  check(r.status === 307 && r.headers.get("location")?.includes("/login"), "pages redirect to /login without a session", String(r.status));
  r = await req("GET", "/api/v1/stats");
  check(r.status === 401, "API refuses requests without a session", String(r.status));
  r = await req("GET", "/api/recordings/00000000-0000-0000-0000-000000000000");
  check(r.status === 401, "recordings need a session", String(r.status));
  r = await req("GET", "/api/kb-files/00000000-0000-0000-0000-000000000000/00000000-0000-0000-0000-000000000000");
  check(r.status === 401, "knowledge files need a session", String(r.status));
  r = await req("POST", "/api/auth/login", { body: { password: "wrong-password" } });
  check(r.status === 401, "wrong password is rejected");
  await sleep(1100); // the login throttle allows one failure per second
  check(Boolean(password), "DASHBOARD_PASSWORD is available to the test");
  r = await req("POST", "/api/auth/login", { body: { password } });
  check(r.status === 200, "correct password signs in", String(r.status));
  cookie = (r.headers.get("set-cookie") ?? "").split(";")[0];
  check(cookie.startsWith("vd_session="), "session cookie is set");
  check(/httponly/i.test(r.headers.get("set-cookie") ?? ""), "session cookie is HttpOnly");
}

// --- pages ---------------------------------------------------------------------
for (const p of ["/", "/calls", "/calls/new", "/agents", "/agents/new", "/agents/import", "/knowledge"]) {
  r = await req("GET", p);
  check(r.status === 200, `page ${p} renders`, String(r.status));
}

// --- read endpoints -------------------------------------------------------------
r = await req("GET", "/api/v1/stats?days=30");
check(r.status === 200 && Array.isArray(r.json?.daily) && r.json.daily.length === 30, "stats: 30 daily points");
check(typeof r.json?.cost?.total_incl_gst === "number", "stats: total spend incl. GST");
r = await req("GET", "/api/v1/options");
check(r.status === 200 && r.json.personas.length > 0 && r.json.languages.length > 0, "options: personas and languages");
r = await req("GET", "/api/v1/calls?limit=5");
check(r.status === 200 && Array.isArray(r.json?.calls), "calls list");
check(r.json.calls.every((c) => /^\*+\d{4}$/.test(c.to)), "calls list masks phone numbers to the last 4 digits");
if (r.json.calls[0]) {
  const id = r.json.calls[0].call_id;
  const d = await req("GET", `/api/v1/calls/${id}`);
  check(d.status === 200 && Array.isArray(d.json?.transcript), "call detail with transcript");
  check(Array.isArray(d.json.lessons) && "review" in d.json && "rating" in d.json, "call detail has review, lessons and rating");
  if (d.json.recording_url) {
    const rec = await req("GET", `/api/recordings/${id}`);
    check(rec.status === 302 && Boolean(rec.headers.get("location")), "recording link redirects to a fresh signed URL", String(rec.status));
    const dl = await req("GET", `/api/recordings/${id}?download=1`);
    check(dl.status === 200 && /attachment/.test(dl.headers.get("content-disposition") ?? ""), "recording downloads as a file", String(dl.status));
  }
  const page = await req("GET", `/calls/${id}`);
  check(page.status === 200, "call detail page renders");
}
r = await req("POST", "/api/v1/agents", { body: { name: "x", prompt: "y" }, headers: { origin: "https://evil.example" } });
check(r.status === 403, "cross-site writes are refused");

// --- knowledge base: upload -> embed -> search -> delete --------------------------
r = await req("POST", "/api/v1/knowledge-bases", { body: { name: `e2e ${new Date().toISOString()}` } });
check(r.status === 201, "knowledge base created", r.text);
const kb = r.json.id;
const doc = `# Sunrise Dental FAQ

## Timings
We are open Monday to Saturday, 9 AM to 8 PM. On Sundays we open only from 10 AM to 1 PM.

## Root canal
A root canal costs between 4,500 and 7,000 rupees depending on the tooth. It takes two visits.

## Parking
Free parking is available in the basement for two hours.
`;
const form = new FormData();
form.append("file", new Blob([doc], { type: "text/markdown" }), "sunrise-faq.md");
r = await req("POST", `/api/v1/knowledge-bases/${kb}/documents`, { raw: form });
check(r.status === 202 && r.json?.status === "pending", "document upload accepted", r.text);
let docs;
for (let i = 0; i < 60; i++) {
  docs = (await req("GET", `/api/v1/knowledge-bases/${kb}/documents`)).json.documents;
  if (docs[0] && ["ready", "failed"].includes(docs[0].status)) break;
  await sleep(1000);
}
check(docs[0]?.status === "ready", "document embedded and ready", JSON.stringify(docs[0]));
check(docs[0].chunks >= 1, `document split into ${docs[0].chunks} chunk(s)`);
r = await req("GET", `/api/v1/knowledge-bases/${kb}/documents/${docs[0].id}`);
check(r.status === 200 && /root canal/i.test(r.json?.text ?? "") && r.json.chunks.length === docs[0].chunks, "document opens with its text and chunks", r.text.slice(0, 200));
check(typeof r.json.file_url === "string", "document has a link to the original file");
r = await req("POST", `/api/v1/knowledge-bases/${kb}/search`, { body: { query: "how much is a root canal?", top_k: 3 } });
check(r.status === 200 && r.json.results.length > 0, "vector search returns results");
check(/root canal/i.test(r.json.results[0].content), "best match is the root-canal passage", r.json.results[0]?.content?.slice(0, 80));
const kbs = (await req("GET", "/api/v1/knowledge-bases")).json.knowledge_bases;
check(kbs.find((k) => k.id === kb)?.chunks === docs[0].chunks, "knowledge base list counts the chunks");
r = await req("GET", `/knowledge/${kb}`);
check(r.status === 200, "knowledge base page renders");

// --- agents -------------------------------------------------------------------------
const agentBody = {
  name: "E2E dental agent",
  description: "created by scripts/e2e.mjs",
  prompt: "You are Priya from Sunrise Dental. Book check-ups.",
  greeting: "I'm calling from Sunrise Dental about your check-up. Is now a good time?",
  knowledge_base_id: kb,
  language: "en-IN",
  booking: false,
  max_duration_secs: 120,
  fields: [
    { key: "insurance", label: "Has dental insurance", type: "boolean", required: true },
    { key: "pain_level", label: "Pain level", type: "choice", options: ["none", "mild", "severe"], description: "" },
  ],
};
r = await req("POST", "/api/v1/agents", { body: agentBody });
check(r.status === 201 && r.json.fields.length === 2, "agent created with fields", r.text);
const agent = r.json.id;
r = await req("POST", "/api/v1/agents", { body: { ...agentBody, fields: [{ key: "Bad Key", label: "x" }] } });
check(r.status === 400, "invalid field key is rejected");
r = await req("PUT", `/api/v1/agents/${agent}`, { body: { ...agentBody, name: "E2E dental agent v2" } });
check(r.status === 200 && r.json.name === "E2E dental agent v2", "agent updated");
r = await req("GET", "/api/v1/agents");
check(r.json.agents.some((a) => a.id === agent && a.knowledge_base_id === kb), "agent listed with its knowledge base");
r = await req("GET", `/agents/${agent}`);
check(r.status === 200, "agent edit page renders");

// --- training -------------------------------------------------------------------------
r = await req("GET", `/api/v1/agents/${agent}/training`);
check(r.status === 200 && r.json.training_notes === "" && r.json.max_chars === 8000, "new agent has empty training notes", r.text);
const notes = "## Lessons from calls\n\n- Ask for the budget before ending the call.\n";
r = await req("PUT", `/api/v1/agents/${agent}/training`, { body: { training_notes: notes, auto_apply_lessons: true } });
check(r.status === 200 && r.json.training_notes.includes("budget") && r.json.auto_apply_lessons === true, "training notes and auto-apply saved", r.text);
r = await req("PUT", `/api/v1/agents/${agent}/training`, { body: { training_notes: "x".repeat(8001) } });
check(r.status === 400, "training notes over 8000 characters are refused", String(r.status));
r = await req("GET", `/api/v1/agents/${agent}/train.md`);
check(r.status === 200 && r.text.includes("budget") && /markdown/.test(r.headers.get("content-type") ?? ""), "train.md downloads as Markdown");
r = await req("GET", `/api/v1/agents/${agent}/lessons`);
check(r.status === 200 && Array.isArray(r.json.lessons) && r.json.lessons.length === 0, "new agent has no lessons");
r = await req("GET", `/api/v1/agents/${agent}/lessons?status=bogus`);
check(r.status === 400, "unknown lesson status is refused");
r = await req("GET", "/api/v1/agents");
check(r.json.agents.find((a) => a.id === agent)?.suggested_lessons === 0, "agent list counts lessons to review");
r = await req("POST", "/api/v1/agents/prompt-edit", { body: { prompt: "You are Priya.", instruction: "" } });
check(r.status === 400, "AI prompt edit refuses an empty instruction", String(r.status));

// --- clean up -----------------------------------------------------------------------
r = await req("DELETE", `/api/v1/agents/${agent}`);
check(r.status === 204, "agent deleted");
r = await req("DELETE", `/api/v1/knowledge-bases/${kb}/documents/${docs[0].id}`);
check(r.status === 204, "document deleted");
r = await req("DELETE", `/api/v1/knowledge-bases/${kb}`);
check(r.status === 204, "knowledge base deleted");

if (cookie) {
  r = await req("POST", "/api/auth/logout");
  check(r.status === 200, "sign out");
}
console.log(`\n${passed} checks passed against ${BASE}`);
