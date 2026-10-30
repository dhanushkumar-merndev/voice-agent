"use client";

import { useState } from "react";
import Link from "next/link";
import { ArrowRightIcon, PhoneCallIcon } from "lucide-react";

import { BarList, DailyCallsChart, DailySpendChart } from "@/components/charts";
import { EmptyState, ErrorAlert, InterestBadge, LoadingBlock, Money, PageHeader, StatTile, StatusBadge } from "@/components/common";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { useApi } from "@/hooks/use-api";
import { compact, duration, humanize, inr, languageName, ms, percent, relative, shortDate } from "@/lib/format";
import type { CallList, Stats } from "@/lib/types";

const RANGES = [
  { days: "7", label: "Last 7 days" },
  { days: "30", label: "Last 30 days" },
  { days: "90", label: "Last 90 days" },
];

export default function DashboardPage() {
  const [days, setDays] = useState("30");
  const stats = useApi<Stats>(`stats?days=${days}`, { refreshMs: 30_000 });
  const recent = useApi<CallList>("calls?limit=8", { refreshMs: (d) => (d?.calls.some((c) => c.in_progress) ? 3000 : 15_000) });
  const s = stats.data;

  return (
    <>
      <PageHeader
        title="Dashboard"
        description={s ? `${shortDate(s.from)} – ${shortDate(s.to)} · India time` : "Calls, spend and quality at a glance"}
        actions={
          <Select value={days} onValueChange={setDays}>
            <SelectTrigger className="w-40" aria-label="Date range">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {RANGES.map((r) => (
                <SelectItem key={r.days} value={r.days}>
                  {r.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        }
      />

      {stats.error && !s && <ErrorAlert error={stats.error} title="Could not load stats" />}
      {stats.loading && <LoadingBlock rows={4} />}

      {s && (
        <div className="grid gap-4">
          <div className="grid gap-4 md:grid-cols-[1.3fr_2fr] [&>*]:min-w-0">
            <StatTile
              hero
              label="Total spend (before GST)"
              value={<Money excl={s.cost.total_excl_gst} incl={s.cost.total_incl_gst} gstPercent={s.gst_percent} />}
              sub={
                <>
                  Hover for the amount with {s.gst_percent}% GST · number rent {inr(s.cost.number_rental_per_month)}/month not included
                  {s.cost.telephony_pending_calls > 0 && ` · ${s.cost.telephony_pending_calls} phone charge(s) pending`}
                </>
              }
            />
            <div className="grid grid-cols-2 gap-4 lg:grid-cols-3">
              <StatTile
                label="Calls"
                value={compact(s.totals.calls)}
                sub={`${s.totals.answered} answered · ${percent(s.totals.answer_rate)}`}
              />
              <StatTile
                label="Talk time"
                value={`${compact(Math.round(s.totals.talk_minutes))} min`}
                sub={`avg ${duration(s.totals.avg_call_secs)} per answered call`}
              />
              <StatTile
                label="Cost per talk minute"
                value={inr(s.cost.per_talk_minute_excl_gst)}
                sub={`before GST · ${inr(s.cost.per_answered_call_excl_gst)} per answered call`}
              />
              <StatTile label="Appointments booked" value={s.totals.appointments_booked} />
              <StatTile
                label="Response time (median)"
                value={ms(s.latency_ms.voice_to_voice.p50)}
                sub={`p95 ${ms(s.latency_ms.voice_to_voice.p95)} · ${s.latency_ms.turns} turns`}
              />
              <StatTile label="Live now" value={s.totals.active_calls} sub="calls in progress" />
            </div>
          </div>

          <div className="grid gap-4 lg:grid-cols-2 [&>*]:min-w-0">
            <Card>
              <CardHeader>
                <CardTitle>Daily spend</CardTitle>
                <CardDescription>₹ before GST, AI services and phone calls</CardDescription>
              </CardHeader>
              <CardContent>
                <DailySpendChart daily={s.daily} />
              </CardContent>
            </Card>
            <Card>
              <CardHeader>
                <CardTitle>Calls per day</CardTitle>
                <CardDescription>Every call placed, answered or not</CardDescription>
              </CardHeader>
              <CardContent>
                <DailyCallsChart daily={s.daily} />
              </CardContent>
            </Card>
          </div>

          <div className="grid gap-4 lg:grid-cols-3 [&>*]:min-w-0">
            <Card>
              <CardHeader>
                <CardTitle>Where the money goes</CardTitle>
                <CardDescription>Before GST, {s.days} days</CardDescription>
              </CardHeader>
              <CardContent>
                <CostBreakdown s={s} />
              </CardContent>
            </Card>
            <Card>
              <CardHeader>
                <CardTitle>Outcomes</CardTitle>
                <CardDescription>How calls ended</CardDescription>
              </CardHeader>
              <CardContent className="grid gap-6">
                <BarList items={s.by_status.map((k) => ({ label: humanize(k.key), value: k.calls }))} />
                <div>
                  <p className="mb-3 text-sm font-medium">Lead interest</p>
                  <BarList
                    items={s.by_interest
                      .filter((k) => k.key !== "unknown")
                      .map((k) => ({ label: k.key === "none" ? "Not interested" : humanize(k.key), value: k.calls }))}
                    emptyText="No leads extracted yet"
                  />
                </div>
              </CardContent>
            </Card>
            <Card>
              <CardHeader>
                <CardTitle>Response time</CardTitle>
                <CardDescription>Per turn, caller stops → agent speaks</CardDescription>
              </CardHeader>
              <CardContent className="grid gap-6">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Stage</TableHead>
                      <TableHead className="text-right">Median</TableHead>
                      <TableHead className="text-right">p95</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {[
                      ["Voice to voice", s.latency_ms.voice_to_voice],
                      ["Speech-to-text final", s.latency_ms.stt_final],
                      ["LLM first token", s.latency_ms.llm_first_token],
                      ["Voice first byte", s.latency_ms.tts_first_byte],
                    ].map(([label, p]) => {
                      const v = p as Stats["latency_ms"]["stt_final"];
                      return (
                        <TableRow key={label as string}>
                          <TableCell>{label as string}</TableCell>
                          <TableCell className="text-right tabular-nums">{ms(v.p50)}</TableCell>
                          <TableCell className="text-right tabular-nums">{ms(v.p95)}</TableCell>
                        </TableRow>
                      );
                    })}
                  </TableBody>
                </Table>
                <div>
                  <p className="mb-3 text-sm font-medium">Languages</p>
                  <BarList items={s.by_language.map((k) => ({ label: languageName(k.key), value: k.calls }))} />
                </div>
              </CardContent>
            </Card>
          </div>

          <Card>
            <CardHeader>
              <CardTitle>Agents</CardTitle>
              <CardDescription>Calls, talk time and spend (before GST) per agent</CardDescription>
              <CardAction>
                <Button asChild variant="outline" size="sm">
                  <Link href="/agents">Manage agents</Link>
                </Button>
              </CardAction>
            </CardHeader>
            <CardContent>
              {s.by_agent.length === 0 ? (
                <p className="py-6 text-center text-sm text-muted-foreground">No calls in this period</p>
              ) : (
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Agent</TableHead>
                      <TableHead className="text-right">Calls</TableHead>
                      <TableHead className="text-right">Answered</TableHead>
                      <TableHead className="text-right">Talk min</TableHead>
                      <TableHead className="text-right">Booked</TableHead>
                      <TableHead className="text-right" title="Average call review score out of 10">Review</TableHead>
                      <TableHead className="text-right">Spend</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {s.by_agent.map((a) => (
                      <TableRow key={a.agent_id ?? a.name}>
                        <TableCell className="font-medium">
                          {a.agent_id ? <Link className="hover:underline" href={`/agents/${a.agent_id}`}>{a.name}</Link> : <span className="text-muted-foreground">{a.name}</span>}
                        </TableCell>
                        <TableCell className="text-right tabular-nums">{a.calls}</TableCell>
                        <TableCell className="text-right tabular-nums">{a.answered}</TableCell>
                        <TableCell className="text-right tabular-nums">{a.talk_minutes}</TableCell>
                        <TableCell className="text-right tabular-nums">{a.appointments}</TableCell>
                        <TableCell className="text-right tabular-nums">{a.avg_review_score !== null ? `${a.avg_review_score}/10` : "—"}</TableCell>
                        <TableCell className="text-right tabular-nums">
                          <Money excl={a.cost_excl_gst} incl={a.cost_incl_gst} gstPercent={s.gst_percent} />
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              )}
            </CardContent>
          </Card>
        </div>
      )}

      <Card className="mt-4">
        <CardHeader>
          <CardTitle>Recent calls</CardTitle>
          <CardAction>
            <Button asChild variant="ghost" size="sm">
              <Link href="/calls">
                All calls <ArrowRightIcon data-icon="inline-end" />
              </Link>
            </Button>
          </CardAction>
        </CardHeader>
        <CardContent>
          {recent.error && !recent.data && <ErrorAlert error={recent.error} />}
          {recent.loading && <LoadingBlock rows={2} />}
          {recent.data?.calls.length === 0 && (
            <EmptyState icon={PhoneCallIcon} title="No calls yet">
              Place your first call with the New call button.
            </EmptyState>
          )}
          <ul className="divide-y">
            {recent.data?.calls.map((c) => (
              <li key={c.call_id}>
                <Link href={`/calls/${c.call_id}`} className="flex flex-wrap items-center gap-x-4 gap-y-1 py-3 hover:bg-muted/40 sm:flex-nowrap">
                  <StatusBadge status={c.status} live={c.in_progress} />
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-sm font-medium">
                      {c.lead_name} <span className="font-normal text-muted-foreground">{c.to}</span>
                    </p>
                    <p className="truncate text-xs text-muted-foreground">{c.summary ?? c.agent_name ?? humanize(c.end_reason)}</p>
                  </div>
                  <InterestBadge level={c.interest_level} />
                  <span className="w-16 text-right text-sm tabular-nums">{duration(c.duration_secs)}</span>
                  <span className="w-16 text-right text-sm tabular-nums">
                    <Money excl={c.cost_excl_gst} incl={c.cost_incl_gst} />
                  </span>
                  <span className="w-24 text-right text-xs text-muted-foreground">{relative(c.created_at)}</span>
                </Link>
              </li>
            ))}
          </ul>
        </CardContent>
      </Card>
    </>
  );
}

function CostBreakdown({ s }: { s: Stats }) {
  const total = s.cost.total_excl_gst || 1;
  const items = [
    { label: "Text-to-speech", value: s.cost.ai.tts },
    { label: "Phone calls (Plivo)", value: s.cost.telephony },
    { label: "Speech-to-text", value: s.cost.ai.stt },
    { label: "LLM (live replies)", value: s.cost.ai.llm },
    { label: "Re-transcription", value: s.cost.ai.stt_retranscribe },
    { label: "Lead extraction", value: s.cost.ai.extraction },
    { label: "Call reviews", value: s.cost.ai.review },
    { label: "Greeting audio", value: s.cost.ai.tts_greeting },
    { label: "Cached voice clips", value: s.cost.other.tts_cache },
    { label: "AI prompt edits", value: s.cost.other.prompt_edit },
  ]
    .filter((i) => i.value > 0)
    .sort((a, b) => b.value - a.value)
    .map((i) => ({ ...i, hint: `${Math.round((i.value / total) * 100)}%` }));
  return (
    <div className="grid gap-4">
      <BarList items={items} format={(v) => inr(v)} />
      <div className="flex justify-between border-t pt-3 text-sm">
        <span className="text-muted-foreground">
          Total before GST · {compact(Math.round(s.totals.stt_seconds / 60))} STT min · {compact(s.totals.tts_chars)} TTS chars
        </span>
        <span className="font-medium tabular-nums">{inr(s.cost.total_excl_gst)}</span>
      </div>
    </div>
  );
}
