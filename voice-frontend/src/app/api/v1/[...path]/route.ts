import { type NextRequest } from "next/server";

import { BACKEND_URL, backendApiKey } from "@/lib/server-config";

/**
 * Forwards /api/v1/* to the voice backend's /v1/*, adding the API key on the
 * server so it never reaches the browser. The session check happens in
 * src/proxy.ts before this runs.
 */
async function forward(request: NextRequest, ctx: RouteContext<"/api/v1/[...path]">) {
  const key = backendApiKey();
  if (!key) {
    return Response.json(
      { error: { code: "not_configured", message: "VOICE_API_KEY is not set for the dashboard server" } },
      { status: 503 },
    );
  }
  // Only same-site pages may change data (the session cookie is SameSite=Lax
  // already; this also covers top-level form posts).
  if (request.method !== "GET") {
    const origin = request.headers.get("origin");
    if (origin && new URL(origin).host !== request.headers.get("host")) {
      return Response.json({ error: { code: "forbidden", message: "cross-site request" } }, { status: 403 });
    }
  }
  const { path } = await ctx.params;
  if (path.some((p) => p === ".." || p.includes("/"))) {
    return Response.json({ error: { code: "bad_request", message: "bad path" } }, { status: 400 });
  }
  const url = `${BACKEND_URL}/v1/${path.map(encodeURIComponent).join("/")}${request.nextUrl.search}`;
  const headers = new Headers({ "x-api-key": key });
  for (const h of ["content-type", "idempotency-key", "accept"]) {
    const v = request.headers.get(h);
    if (v) headers.set(h, v);
  }
  const hasBody = !["GET", "HEAD"].includes(request.method);
  try {
    const upstream = await fetch(url, {
      method: request.method,
      headers,
      body: hasBody ? await request.arrayBuffer() : undefined,
      cache: "no-store",
      signal: AbortSignal.timeout(60_000),
    });
    const out = new Headers();
    const type = upstream.headers.get("content-type");
    if (type) out.set("content-type", type);
    out.set("cache-control", "no-store");
    return new Response(upstream.status === 204 ? null : upstream.body, {
      status: upstream.status,
      headers: out,
    });
  } catch (e) {
    const message = e instanceof Error && e.name === "TimeoutError" ? "the voice backend timed out" : "the voice backend is not reachable";
    return Response.json({ error: { code: "unavailable", message } }, { status: 502 });
  }
}

export const GET = forward;
export const POST = forward;
export const PUT = forward;
export const DELETE = forward;
