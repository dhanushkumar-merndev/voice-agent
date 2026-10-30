import { type NextRequest } from "next/server";

import { BACKEND_URL, backendApiKey } from "@/lib/server-config";

const CALL_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/**
 * A call recording at a link that does not expire. The backend only hands
 * out signed storage links that last a few minutes, so every request asks it
 * for a fresh one. Playback redirects to that link (the storage server
 * answers range requests for seeking); `?download=1` streams the file with
 * a filename instead. The session check happens in src/proxy.ts.
 */
export async function GET(request: NextRequest, ctx: RouteContext<"/api/recordings/[id]">) {
  const { id } = await ctx.params;
  if (!CALL_ID.test(id)) {
    return Response.json({ error: { code: "bad_request", message: "bad call id" } }, { status: 400 });
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
    const res = await fetch(`${BACKEND_URL}/v1/calls/${id}`, {
      headers: { "x-api-key": key },
      cache: "no-store",
      signal: AbortSignal.timeout(15_000),
    });
    if (!res.ok) {
      return new Response(res.body, { status: res.status, headers: { "content-type": "application/json" } });
    }
    signed = ((await res.json()) as { recording_url?: string | null }).recording_url ?? null;
  } catch {
    return Response.json({ error: { code: "unavailable", message: "the voice backend is not reachable" } }, { status: 502 });
  }
  if (!signed) {
    return Response.json({ error: { code: "not_found", message: "this call has no recording yet" } }, { status: 404 });
  }

  if (request.nextUrl.searchParams.get("download") === null) {
    return new Response(null, { status: 302, headers: { location: signed, "cache-control": "no-store" } });
  }
  try {
    const file = await fetch(signed, { cache: "no-store", signal: AbortSignal.timeout(60_000) });
    if (!file.ok || !file.body) {
      return Response.json({ error: { code: "unavailable", message: "recording storage did not answer" } }, { status: 502 });
    }
    const headers = new Headers({
      "content-type": file.headers.get("content-type") ?? "audio/mpeg",
      "content-disposition": `attachment; filename="call-${id}.mp3"`,
      "cache-control": "no-store",
    });
    const length = file.headers.get("content-length");
    if (length) headers.set("content-length", length);
    return new Response(file.body, { status: 200, headers });
  } catch {
    return Response.json({ error: { code: "unavailable", message: "recording storage is not reachable" } }, { status: 502 });
  }
}
