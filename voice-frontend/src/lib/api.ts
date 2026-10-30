/** Browser-side client for the dashboard's /api/v1 proxy. */

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
    public code?: string,
  ) {
    super(message);
  }
}

async function parse<T>(res: Response): Promise<T> {
  if (res.status === 204) return undefined as T;
  const body = await res.json().catch(() => null);
  if (!res.ok) {
    if (res.status === 401 && typeof window !== "undefined" && body?.error?.code === "unauthorized") {
      // A full page load, so no stale client state survives the sign-out.
      // eslint-disable-next-line @next/next/no-location-assign-relative-destination
      window.location.assign(`/login?next=${encodeURIComponent(window.location.pathname)}`);
    }
    throw new ApiError(res.status, body?.error?.message ?? `Request failed (${res.status})`, body?.error?.code);
  }
  return body as T;
}

/** GET /api/v1/<path>. */
export async function apiGet<T>(path: string, signal?: AbortSignal): Promise<T> {
  return parse<T>(await fetch(`/api/v1/${path}`, { cache: "no-store", signal }));
}

/** POST/PUT/DELETE with a JSON body. POSTs carry a fresh Idempotency-Key.
 *  Gives up after `timeoutMs` (30 s) so a stuck request never leaves a
 *  button spinning. */
export async function apiSend<T>(method: "POST" | "PUT" | "DELETE", path: string, body?: unknown, timeoutMs = 30_000): Promise<T> {
  const headers: Record<string, string> = {};
  if (body !== undefined) headers["content-type"] = "application/json";
  if (method === "POST") headers["idempotency-key"] = crypto.randomUUID();
  let res: Response;
  try {
    res = await fetch(`/api/v1/${path}`, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(timeoutMs),
    });
  } catch (e) {
    if (e instanceof Error && e.name === "TimeoutError") {
      throw new ApiError(0, `No answer from the server in ${Math.round(timeoutMs / 1000)} seconds. It may still have gone through, so check before trying again.`);
    }
    throw new ApiError(0, "Could not reach the dashboard server. Check your connection and try again.");
  }
  return parse<T>(res);
}

/** Uploads one file (multipart field `file`), reporting progress 0-1. */
export function apiUpload<T>(path: string, file: File, onProgress?: (p: number) => void): Promise<T> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", `/api/v1/${path}`);
    xhr.setRequestHeader("idempotency-key", crypto.randomUUID());
    xhr.upload.onprogress = (e) => e.lengthComputable && onProgress?.(e.loaded / e.total);
    xhr.onload = () => {
      let body: { error?: { message?: string; code?: string } } | null = null;
      try {
        body = JSON.parse(xhr.responseText);
      } catch {}
      if (xhr.status >= 200 && xhr.status < 300) resolve(body as T);
      else reject(new ApiError(xhr.status, body?.error?.message ?? `Upload failed (${xhr.status})`, body?.error?.code));
    };
    xhr.onerror = () => reject(new ApiError(0, "Network error during upload"));
    const form = new FormData();
    form.append("file", file);
    xhr.send(form);
  });
}

export function errorMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
