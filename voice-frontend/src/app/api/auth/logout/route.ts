import { NextResponse } from "next/server";

import { SESSION_COOKIE } from "@/lib/session";

/** POST: clears the session cookie. */
export async function POST() {
  const response = NextResponse.json({ ok: true });
  response.cookies.delete(SESSION_COOKIE);
  return response;
}
