"use client";

import { useCallback, useEffect, useRef, useState } from "react";

import { apiGet } from "@/lib/api";

interface Options<T> {
  /** Re-fetch every N ms (paused while the tab is hidden). */
  refreshMs?: number | ((data: T | undefined) => number | undefined);
  /** Skip fetching (e.g. a dependency is not ready). */
  enabled?: boolean;
}

/** Fetches /api/v1/<path>, keeps the last good data across refreshes. */
export function useApi<T>(path: string | null, { refreshMs, enabled = true }: Options<T> = {}) {
  const [data, setData] = useState<T>();
  const [error, setError] = useState<Error>();
  const [loading, setLoading] = useState(true);
  const dataRef = useRef<T | undefined>(undefined);
  const [tick, setTick] = useState(0);
  // Kept in a ref so an inline function does not restart the fetch loop.
  const refreshRef = useRef(refreshMs);
  useEffect(() => {
    refreshRef.current = refreshMs;
  });

  const reload = useCallback(() => setTick((t) => t + 1), []);

  useEffect(() => {
    if (!path || !enabled) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    let stopped = false;
    const run = async () => {
      try {
        if (document.visibilityState !== "hidden") {
          const next = await apiGet<T>(path, controller.signal);
          dataRef.current = next;
          setData(next);
          setError(undefined);
        }
      } catch (e) {
        if (!controller.signal.aborted) setError(e instanceof Error ? e : new Error(String(e)));
      } finally {
        if (!controller.signal.aborted) setLoading(false);
      }
      const refresh = refreshRef.current;
      const every = typeof refresh === "function" ? refresh(dataRef.current) : refresh;
      if (every && !stopped) timer = setTimeout(run, every);
    };
    void run();
    return () => {
      stopped = true;
      controller.abort();
      if (timer) clearTimeout(timer);
    };
  }, [path, enabled, tick]);

  return { data, error, loading: loading && data === undefined, reload, setData };
}
