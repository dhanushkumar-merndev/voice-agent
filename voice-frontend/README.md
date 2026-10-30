# Voice Agents dashboard

Next.js 16 + shadcn/ui front end for the Rust voice backend (`../voice-backend`).

- **Dashboard**: spend incl. GST, calls, talk time, ₹ per talk minute, bookings, response time, daily spend and calls, cost by service, outcomes, lead interest, languages, per-agent table.
- **Calls**: filterable list, the new-call form, and a live call page (transcript, lead fields, recording, cost breakdown, per-turn latency, hang up).
- **Agents**: one character per kind of call. Each has a prompt, details to collect (typed fields that are filled in after the call), a knowledge base, a language and defaults.
- **Knowledge bases**: drag-and-drop PDF, Markdown or text uploads. They are chunked, embedded with bge-m3 and stored in pgvector, and a test-search box shows what the agent would find.

The browser only talks to this app. Route handlers under `/api/v1/*` forward to the backend and add the `X-API-Key` on the server, so the key never reaches the browser. A password sign-in with an HttpOnly session cookie protects every page and API route (`src/proxy.ts`).

## Run

```bash
cp ../.env.example ../.env      # once, if missing; set DASHBOARD_PASSWORD and SESSION_SECRET
pnpm install
pnpm build && pnpm start -p 3100        # or: pnpm dev
```

The voice backend must be running (`VOICE_API_URL`, default `http://127.0.0.1:8080`). Both apps share the root `../.env` (loaded in `next.config.ts`); the API key is the backend's `API_KEY`, or `VOICE_API_KEY` to override it.

## Test

```bash
node scripts/e2e.mjs http://127.0.0.1:3100
```

This signs in and checks every page and API route through the proxy: phone masking, cross-site write refusal, knowledge-base upload → embedding → vector search, and agent create/edit/delete. It cleans up after itself.

Also run `pnpm lint` and `pnpm exec tsc --noEmit`.
