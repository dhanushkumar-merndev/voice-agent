import "server-only";

import { readFileSync } from "node:fs";
import path from "node:path";

/** Where the Rust voice backend listens (server-side only). */
export const BACKEND_URL = (process.env.VOICE_API_URL ?? "http://127.0.0.1:8080").replace(/\/+$/, "");

const isProduction = process.env.NODE_ENV === "production";

let cachedKey: string | undefined;

/**
 * The backend's X-API-Key. Taken from VOICE_API_KEY, else from API_KEY in the
 * env file named by VOICE_BACKEND_ENV (in development, ../voice-backend/.env
 * by default), so the key can live in one place. Never sent to the browser.
 */
export function backendApiKey(): string | undefined {
  if (process.env.VOICE_API_KEY) return process.env.VOICE_API_KEY;
  if (isProduction && !process.env.VOICE_BACKEND_ENV) return undefined;
  if (cachedKey !== undefined) return cachedKey || undefined;
  const file = process.env.VOICE_BACKEND_ENV ?? path.join(process.cwd(), "..", "voice-backend", ".env");
  try {
    const line = readFileSync(file, "utf8")
      .split("\n")
      .find((l) => l.startsWith("API_KEY="));
    cachedKey = line ? line.slice("API_KEY=".length).trim().replace(/^["']|["']$/g, "") : "";
  } catch {
    cachedKey = "";
  }
  return cachedKey || undefined;
}
