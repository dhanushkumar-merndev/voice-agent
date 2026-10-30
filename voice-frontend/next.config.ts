import { readFileSync } from "node:fs";
import path from "node:path";
import { parseEnv } from "node:util";
import type { NextConfig } from "next";

// Both apps share one env file at the repo root (../.env). Variables already in
// the environment win, and a missing file is fine (production sets real ones).
try {
  const shared = parseEnv(readFileSync(path.join(import.meta.dirname, "..", ".env"), "utf8"));
  for (const [key, value] of Object.entries(shared)) process.env[key] ??= value;
} catch {}

const nextConfig: NextConfig = {
  /* config options here */
  cacheComponents: true,
  partialPrefetching: true,
  turbopack: {
    rules: {
      "*.css": {
        loaders: ["@tailwindcss/turbopack"],
        as: "*.css",
      },
    },
  },
};

export default nextConfig;
