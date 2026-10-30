import { NextResponse, type NextRequest } from "next/server";

import { SESSION_COOKIE, SESSION_HOURS, authMode, createSession, safeEqual } from "@/lib/session";

// Slows password guessing: one failed attempt per IP per second.
const lastFailure = new Map<string, number>();

/** POST {password}: sets the session cookie when the password matches. */
export async function POST(request: NextRequest) {
  const mode = authMode();
  if (mode === "misconfigured") {
    return NextResponse.json(
      { error: { message: "DASHBOARD_PASSWORD is not set on the server" } },
      { status: 503 },
    );
  }
  const ip = request.headers.get("x-forwarded-for")?.split(",")[0]?.trim() || "local";
  const last = lastFailure.get(ip) ?? 0;
  if (Date.now() - last < 1000) {
    return NextResponse.json({ error: { message: "Too many attempts, wait a moment" } }, { status: 429 });
  }
  const body = (await request.json().catch(() => ({}))) as { password?: unknown };
  const password = typeof body.password === "string" ? body.password : "";
  if (mode === "password" && !safeEqual(password, process.env.DASHBOARD_PASSWORD ?? "")) {
    lastFailure.set(ip, Date.now());
    return NextResponse.json({ error: { message: "Wrong password" } }, { status: 401 });
  }
  const response = NextResponse.json({ ok: true });
  response.cookies.set(SESSION_COOKIE, await createSession(), {
    httpOnly: true,
    sameSite: "lax",
    secure: process.env.NODE_ENV === "production" && process.env.INSECURE_COOKIES !== "1",
    path: "/",
    maxAge: SESSION_HOURS * 3600,
  });
  return response;
}
