import { BACKEND_URL, backendApiKey } from "@/lib/server-config";

const ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/**
 * The original file of a knowledge-base document, streamed through the
 * dashboard so the browser can read it (agent-pack export) without needing
 * access to the storage server. The session check happens in src/proxy.ts.
 */
export async function GET(_request: Request, ctx: RouteContext<"/api/kb-files/[kb]/[doc]">) {
  const { kb, doc } = await ctx.params;
  if (!ID.test(kb) || !ID.test(doc)) {
    return Response.json({ error: { code: "bad_request", message: "bad document id" } }, { status: 400 });
  }
  const key = backendApiKey();
  if (!key) {
    return Response.json(
      { error: { code: "not_configured", message: "VOICE_API_KEY is not set for the dashboard server" } },
      { status: 503 },
    );
  }
  let signed: string | null = null;
  try {
    const res = await fetch(`${BACKEND_URL}/v1/knowledge-bases/${kb}/documents/${doc}`, {
      headers: { "x-api-key": key },
      cache: "no-store",
      signal: AbortSignal.timeout(15_000),
    });
    if (!res.ok) {
      return new Response(res.body, { status: res.status, headers: { "content-type": "application/json" } });
    }
    signed = ((await res.json()) as { file_url?: string | null }).file_url ?? null;
  } catch {
    return Response.json({ error: { code: "unavailable", message: "the voice backend is not reachable" } }, { status: 502 });
  }
  if (!signed) {
    return Response.json({ error: { code: "not_found", message: "the original file is not stored" } }, { status: 404 });
  }
  try {
    const file = await fetch(signed, { cache: "no-store", signal: AbortSignal.timeout(60_000) });
    if (!file.ok || !file.body) {
      return Response.json({ error: { code: "unavailable", message: "file storage did not answer" } }, { status: 502 });
    }
    const headers = new Headers({
      "content-type": "application/octet-stream",
      "content-disposition": "attachment",
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
    });
    const length = file.headers.get("content-length");
    if (length) headers.set("content-length", length);
    return new Response(file.body, { status: 200, headers });
  } catch {
    return Response.json({ error: { code: "unavailable", message: "file storage is not reachable" } }, { status: 502 });
  }
}
