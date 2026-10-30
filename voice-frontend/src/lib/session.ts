/**
 * Dashboard sessions: a signed, expiring cookie set after the dashboard
 * password is entered. Uses Web Crypto so it runs in the proxy and in route
 * handlers alike.
 */

export const SESSION_COOKIE = "vd_session";
export const SESSION_HOURS = 12;

const encoder = new TextEncoder();

/** Whether a password is required. Without DASHBOARD_PASSWORD the dashboard
 * is open in development and locked in production. */
export function authMode(): "password" | "open" | "misconfigured" {
  if (process.env.DASHBOARD_PASSWORD) return "password";
  return process.env.NODE_ENV === "production" ? "misconfigured" : "open";
}

function secret(): string {
  // SESSION_SECRET rotates sessions independently; the password works as a fallback.
  return process.env.SESSION_SECRET || `vd:${process.env.DASHBOARD_PASSWORD ?? ""}`;
}

async function hmac(data: string): Promise<string> {
  const key = await crypto.subtle.importKey(
    "raw",
    encoder.encode(secret()),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const sig = await crypto.subtle.sign("HMAC", key, encoder.encode(data));
  return Buffer.from(sig).toString("base64url");
}

/** Constant-time string comparison. */
export function safeEqual(a: string, b: string): boolean {
  const x = encoder.encode(a);
  const y = encoder.encode(b);
  let diff = x.length ^ y.length;
  for (let i = 0; i < Math.max(x.length, y.length); i++) diff |= (x[i] ?? 0) ^ (y[i] ?? 0);
  return diff === 0;
}

/** A new session token valid for SESSION_HOURS. */
export async function createSession(): Promise<string> {
  const expires = Date.now() + SESSION_HOURS * 3600_000;
  return `${expires}.${await hmac(String(expires))}`;
}

/** Whether a session token is genuine and unexpired. */
export async function verifySession(token: string | undefined): Promise<boolean> {
  if (!token) return false;
  const [expires, sig] = token.split(".");
  if (!expires || !sig || !/^\d+$/.test(expires) || Number(expires) < Date.now()) return false;
  return safeEqual(sig, await hmac(expires));
}
