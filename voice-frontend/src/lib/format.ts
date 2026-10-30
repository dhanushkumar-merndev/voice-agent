/** Display formatting. Times are shown in India time, money in rupees. */

const TZ = "Asia/Kolkata";

/** ₹ with paise for small amounts, whole rupees past ₹1,000. */
export function inr(v: number | null | undefined, opts: { precise?: boolean } = {}): string {
  if (v === null || v === undefined || Number.isNaN(v)) return "—";
  const digits = opts.precise ? 4 : Math.abs(v) >= 1000 ? 0 : 2;
  return `₹${v.toLocaleString("en-IN", { minimumFractionDigits: digits, maximumFractionDigits: digits })}`;
}

/** Compact counts: 1,284 / 12.9K / 1.2M. */
export function compact(v: number | null | undefined): string {
  if (v === null || v === undefined) return "—";
  return Intl.NumberFormat("en-IN", { notation: v >= 10_000 ? "compact" : "standard", maximumFractionDigits: 1 }).format(v);
}

export function percent(v: number | null | undefined): string {
  return v === null || v === undefined ? "—" : `${Math.round(v * 100)}%`;
}

/** 95 -> "1m 35s". */
export function duration(secs: number | null | undefined): string {
  if (secs === null || secs === undefined) return "—";
  const s = Math.round(secs);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${String(s % 60).padStart(2, "0")}s`;
  return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, "0")}m`;
}

export function ms(v: number | null | undefined): string {
  if (v === null || v === undefined) return "—";
  return v >= 1000 ? `${(v / 1000).toFixed(2)}s` : `${Math.round(v)}ms`;
}

export function dateTime(iso: string | null | undefined): string {
  if (!iso) return "—";
  return new Date(iso).toLocaleString("en-IN", {
    timeZone: TZ,
    day: "numeric",
    month: "short",
    hour: "numeric",
    minute: "2-digit",
  });
}

export function time(iso: string | null | undefined): string {
  if (!iso) return "—";
  return new Date(iso).toLocaleTimeString("en-IN", { timeZone: TZ, hour: "numeric", minute: "2-digit", second: "2-digit" });
}

/** "2026-10-29" -> "29 Oct". */
export function shortDate(day: string): string {
  return new Date(`${day}T00:00:00+05:30`).toLocaleDateString("en-IN", { timeZone: TZ, day: "numeric", month: "short" });
}

export function relative(iso: string | null | undefined): string {
  if (!iso) return "—";
  const diff = (Date.now() - new Date(iso).getTime()) / 1000;
  if (diff < 60) return "just now";
  if (diff < 3600) return `${Math.floor(diff / 60)} min ago`;
  if (diff < 86400) return `${Math.floor(diff / 3600)} h ago`;
  return dateTime(iso);
}

export function bytes(v: number | null | undefined): string {
  if (!v) return "—";
  if (v < 1024) return `${v} B`;
  if (v < 1024 * 1024) return `${(v / 1024).toFixed(1)} KB`;
  return `${(v / 1024 / 1024).toFixed(1)} MB`;
}

const LANGUAGES: Record<string, string> = {
  auto: "Auto-detect",
  "en-IN": "English",
  "hi-IN": "Hindi",
  "kn-IN": "Kannada",
  "ta-IN": "Tamil",
  "te-IN": "Telugu",
  "ml-IN": "Malayalam",
  "bn-IN": "Bengali",
  "mr-IN": "Marathi",
  "gu-IN": "Gujarati",
  "pa-IN": "Punjabi",
  "od-IN": "Odia",
};

export function languageName(code: string | null | undefined): string {
  if (!code) return "—";
  return LANGUAGES[code] ?? code;
}

/** "agent_end_call" -> "Agent end call". */
export function humanize(v: string | null | undefined): string {
  if (!v) return "—";
  const s = v.replace(/_/g, " ");
  return s.charAt(0).toUpperCase() + s.slice(1);
}

/** "Budget (lakh)" -> "budget_lakh". */
export function toKey(label: string): string {
  const k = label
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "")
    .slice(0, 40);
  return /^[a-z]/.test(k) ? k : k ? `f_${k}`.slice(0, 40) : "";
}

/** "9036215854" or "+91 90362 15854" -> "+919036215854" (Indian numbers by default). */
export function toE164(raw: string): string {
  const s = raw.replace(/[\s()-]/g, "");
  if (/^\d{10}$/.test(s)) return `+91${s}`;
  if (/^91\d{10}$/.test(s)) return `+${s}`;
  if (/^0\d{10}$/.test(s)) return `+91${s.slice(1)}`;
  return s;
}
