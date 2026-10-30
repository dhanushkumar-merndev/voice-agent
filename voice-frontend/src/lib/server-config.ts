import "server-only";

/** Where the Rust voice backend listens (server-side only). */
export const BACKEND_URL = (process.env.VOICE_API_URL ?? "http://127.0.0.1:8080").replace(/\/+$/, "");

/**
 * The backend's X-API-Key: VOICE_API_KEY, else the backend's own API_KEY from
 * the shared root .env (loaded in next.config.ts). Never sent to the browser.
 */
export function backendApiKey(): string | undefined {
  return process.env.VOICE_API_KEY || process.env.API_KEY || undefined;
}
