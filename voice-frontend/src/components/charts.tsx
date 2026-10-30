"use client";

import { Bar, BarChart, CartesianGrid, XAxis, YAxis } from "recharts";

import { ChartContainer, ChartLegend, ChartLegendContent, ChartTooltip, ChartTooltipContent, type ChartConfig } from "@/components/ui/chart";
import { inr, ms, shortDate } from "@/lib/format";
import type { Stats, TurnLatency } from "@/lib/types";

// Mark specs (dataviz): bars <= 24px, 4px rounded data end, square at the
// baseline, 2px surface-coloured gap between stacked segments, hairline grid.
const MAX_BAR = 24;
const GAP = { stroke: "var(--card)", strokeWidth: 2 };

const spendConfig = {
  ai: { label: "AI services", color: "var(--chart-1)" },
  phone: { label: "Phone calls", color: "var(--chart-2)" },
} satisfies ChartConfig;

/** Daily spend before GST, AI services and phone stacked. */
export function DailySpendChart({ daily }: { daily: Stats["daily"] }) {
  const data = daily.map((d) => ({ date: d.date, ai: d.ai_cost, phone: d.telephony_cost }));
  return (
    <ChartContainer config={spendConfig} className="aspect-auto h-64 w-full">
      <BarChart data={data} margin={{ left: 4, right: 4, top: 8 }} barCategoryGap="20%">
        <CartesianGrid vertical={false} strokeWidth={1} />
        <XAxis
          dataKey="date"
          tickLine={false}
          axisLine={false}
          tickMargin={8}
          minTickGap={24}
          tickFormatter={shortDate}
        />
        <YAxis tickLine={false} axisLine={false} width={48} tickFormatter={(v: number) => `₹${v}`} />
        <ChartTooltip
          cursor={{ fillOpacity: 0.5 }}
          content={
            <ChartTooltipContent
              labelFormatter={(_, p) => shortDate(String(p?.[0]?.payload?.date ?? ""))}
              formatter={(value, name) => (
                <div className="flex w-full items-center justify-between gap-4">
                  <span className="flex items-center gap-1.5 text-muted-foreground">
                    <span
                      className="size-2.5 rounded-[2px]"
                      style={{ background: `var(--color-${String(name)})` }}
                    />
                    {spendConfig[name as keyof typeof spendConfig]?.label}
                  </span>
                  <span className="font-mono font-medium tabular-nums">{inr(Number(value))}</span>
                </div>
              )}
            />
          }
        />
        <ChartLegend content={<ChartLegendContent />} />
        <Bar dataKey="ai" stackId="spend" fill="var(--color-ai)" maxBarSize={MAX_BAR} isAnimationActive={false} {...GAP} radius={[0, 0, 0, 0]} />
        <Bar dataKey="phone" stackId="spend" fill="var(--color-phone)" maxBarSize={MAX_BAR} isAnimationActive={false} {...GAP} radius={[4, 4, 0, 0]} />
      </BarChart>
    </ChartContainer>
  );
}

const callsConfig = { calls: { label: "Calls", color: "var(--chart-1)" } } satisfies ChartConfig;

/** Calls placed per day (single series: the card title names it). */
export function DailyCallsChart({ daily }: { daily: Stats["daily"] }) {
  return (
    <ChartContainer config={callsConfig} className="aspect-auto h-64 w-full">
      <BarChart data={daily} margin={{ left: 4, right: 4, top: 8 }} barCategoryGap="20%">
        <CartesianGrid vertical={false} strokeWidth={1} />
        <XAxis dataKey="date" tickLine={false} axisLine={false} tickMargin={8} minTickGap={24} tickFormatter={shortDate} />
        <YAxis tickLine={false} axisLine={false} width={32} allowDecimals={false} />
        <ChartTooltip
          cursor={{ fillOpacity: 0.5 }}
          content={
            <ChartTooltipContent
              hideIndicator
              labelFormatter={(_, p) => shortDate(String(p?.[0]?.payload?.date ?? ""))}
              formatter={(_, __, item) => {
                const d = item.payload as Stats["daily"][number];
                return (
                  <div className="grid gap-0.5 tabular-nums">
                    <span>
                      {d.calls} calls · {d.answered} answered
                    </span>
                    <span className="text-muted-foreground">{d.talk_minutes} talk min</span>
                  </div>
                );
              }}
            />
          }
        />
        <Bar dataKey="calls" fill="var(--color-calls)" maxBarSize={MAX_BAR} isAnimationActive={false} radius={[4, 4, 0, 0]} />
      </BarChart>
    </ChartContainer>
  );
}

/**
 * Horizontal bars for one measure across categories, with the value at the
 * bar tip. Plain HTML so labels never clip; a single hue (magnitude).
 */
export function BarList({
  items,
  format = (v) => String(v),
  emptyText = "No data yet",
}: {
  items: { label: string; value: number; hint?: string }[];
  format?: (v: number) => string;
  emptyText?: string;
}) {
  const max = Math.max(0, ...items.map((i) => i.value));
  if (items.length === 0 || max === 0) return <p className="py-6 text-center text-sm text-muted-foreground">{emptyText}</p>;
  return (
    <ul className="grid gap-3">
      {items.map((item) => (
        <li key={item.label} className="group grid gap-1" title={`${item.label}: ${format(item.value)}${item.hint ? ` (${item.hint})` : ""}`}>
          <div className="flex items-baseline justify-between gap-3 text-sm">
            <span className="truncate">{item.label}</span>
            <span className="shrink-0 font-medium tabular-nums">
              {format(item.value)}
              {item.hint && <span className="ml-1.5 font-normal text-muted-foreground">{item.hint}</span>}
            </span>
          </div>
          <div className="h-2 w-full">
            <div
              className="h-full rounded-r-[4px] bg-[var(--chart-1)] transition-[width] group-hover:opacity-80"
              style={{ width: `${Math.max(1.5, (item.value / max) * 100)}%` }}
            />
          </div>
        </li>
      ))}
    </ul>
  );
}

const turnConfig = { v2v: { label: "Voice to voice", color: "var(--chart-1)" } } satisfies ChartConfig;

/** Response time of each agent turn (single series). */
export function TurnLatencyChart({ turns }: { turns: TurnLatency[] }) {
  const data = turns.filter((t) => t.first_audio_sent_ms !== null).map((t, i) => ({ turn: i + 1, v2v: t.first_audio_sent_ms, t }));
  if (data.length === 0) return <p className="py-6 text-center text-sm text-muted-foreground">No timed turns yet</p>;
  return (
    <ChartContainer config={turnConfig} className="aspect-auto h-56 w-full">
      <BarChart data={data} margin={{ left: 4, right: 4, top: 8 }} barCategoryGap="20%">
        <CartesianGrid vertical={false} strokeWidth={1} />
        <XAxis dataKey="turn" tickLine={false} axisLine={false} tickMargin={8} tickFormatter={(v) => `#${v}`} />
        <YAxis tickLine={false} axisLine={false} width={44} tickFormatter={(v: number) => `${(v / 1000).toFixed(1)}s`} />
        <ChartTooltip
          cursor={{ fillOpacity: 0.5 }}
          content={
            <ChartTooltipContent
              hideIndicator
              labelFormatter={(_, p) => `Turn ${p?.[0]?.payload?.turn ?? ""}`}
              formatter={(_, __, item) => {
                const t = (item.payload as { t: TurnLatency }).t;
                return (
                  <div className="grid gap-0.5 tabular-nums">
                    <span className="font-medium">{ms(t.first_audio_sent_ms)} voice to voice</span>
                    <span className="text-muted-foreground">STT {ms(t.stt_final_ms)} · LLM {ms(t.llm_first_token_ms)} · TTS {ms(t.tts_first_byte_ms)}</span>
                    {t.llm_attempts > 1 && <span className="text-muted-foreground">LLM retried ({t.llm_model})</span>}
                  </div>
                );
              }}
            />
          }
        />
        <Bar dataKey="v2v" fill="var(--color-v2v)" maxBarSize={MAX_BAR} isAnimationActive={false} radius={[4, 4, 0, 0]} />
      </BarChart>
    </ChartContainer>
  );
}
