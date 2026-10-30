import { NextResponse, type NextRequest } from "next/server";

import { SESSION_COOKIE, authMode, verifySession } from "@/lib/session";

/** Every page and /api/v1 call needs a dashboard session (see lib/session). */
export async function proxy(request: NextRequest) {
  const mode = authMode();
  if (mode === "open") return NextResponse.next();
  const ok =
    mode === "password" && (await verifySession(request.cookies.get(SESSION_COOKIE)?.value));
  if (ok) return NextResponse.next();
  if (request.nextUrl.pathname.startsWith("/api/")) {
    return NextResponse.json(
      { error: { code: "unauthorized", message: "sign in to the dashboard" } },
      { status: 401 },
    );
  }
  const login = new URL("/login", request.url);
  const next = request.nextUrl.pathname + request.nextUrl.search;
  if (next !== "/") login.searchParams.set("next", next);
  return NextResponse.redirect(login);
}

export const config = {
  matcher: ["/((?!login|api/auth|_next/static|_next/image|favicon.ico|.*\\.(?:svg|png|ico)$).*)"],
};
