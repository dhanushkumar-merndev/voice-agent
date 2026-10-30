"use client";

import { useState } from "react";
import Link from "next/link";
import { CalendarCheckIcon, DownloadIcon, PhoneOffIcon } from "lucide-react";
import { toast } from "sonner";

import { TurnLatencyChart } from "@/components/charts";
import { ErrorAlert, InterestBadge, LoadingBlock, Money, PageHeader, StatusBadge } from "@/components/common";
import { CallReviewCard } from "@/components/training";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Table, TableBody, TableCell, TableRow } from "@/components/ui/table";
import { useApi } from "@/hooks/use-api";
import { apiSend, errorMessage } from "@/lib/api";
import { compact, dateTime, duration, humanize, inr, languageName, ms, time } from "@/lib/format";
import { cn } from "@/lib/utils";
import type { CallDetail, LeadField } from "@/lib/types";

function mask(n: string) {
  return n.length > 4 ? `${n.slice(0, -4).replace(/\d/g, "•")}${n.slice(-4)}` : n;
}

/** Poll fast while live, slower while post-call work (extraction, phone charge, recording) is pending. */
function refreshFor(c: CallDetail | undefined) {
  if (!c) return 3000;
  if (c.in_progress || ["queued", "ringing", "in_progress"].includes(c.status)) return 2000;
  const recent = c.ended_at && Date.now() - new Date(c.ended_at).getTime() < 5 * 60_000;
  const pending = !c.extracted_fields || c.cost.telephony.pending || !c.recording_url || (c.answered_at !== null && !c.review);
  return recent && pending ? 5000 : undefined;
}

export function CallView({ id }: { id: string }) {
  const call = useApi<CallDetail>(`calls/${id}`, { refreshMs: refreshFor });
  const c = call.data;
  const [hangingUp, setHangingUp] = useState(false);

  async function hangup() {
    setHangingUp(true);
    try {
      await apiSend("POST", `calls/${id}/hangup`);
      toast.success("Hanging up");
      call.reload();
    } catch (e) {
      toast.error("Could not hang up", { description: errorMessage(e) });
    } finally {
      setHangingUp(false);
    }
  }

  if (call.loading) return <LoadingBlock rows={5} />;
  if (!c) return <ErrorAlert error={call.error ?? "Call not found"} title="Could not load the call" />;
  const live = c.in_progress || ["queued", "ringing", "in_progress"].includes(c.status);

  return (
    <>
      <PageHeader
        title={c.extracted_fields?.name || c.lead_name}
        description={
          <span className="flex flex-wrap items-center gap-2">
            <StatusBadge status={c.status} live={live} />
            <span className="tabular-nums">{mask(c.to)}</span>
            <span>·</span>
            <span>{dateTime(c.created_at)}</span>
            {c.agent_name && (
              <>
                <span>·</span>
                <Link className="underline" href={`/agents/${c.agent_id}`}>
                  {c.agent_name}
                </Link>
              </>
            )}
          </span>
        }
        actions={
          live ? (
            <AlertDialog>
              <AlertDialogTrigger asChild>
                <Button variant="destructive" disabled={hangingUp}>
                  <PhoneOffIcon data-icon="inline-start" />
                  Hang up
                </Button>
              </AlertDialogTrigger>
              <AlertDialogContent>
                <AlertDialogHeader>
                  <AlertDialogTitle>End this call now?</AlertDialogTitle>
                  <AlertDialogDescription>The caller will be disconnected immediately.</AlertDialogDescription>
                </AlertDialogHeader>
                <AlertDialogFooter>
                  <AlertDialogCancel>Keep talking</AlertDialogCancel>
                  <AlertDialogAction onClick={hangup}>Hang up</AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
          ) : (
            <Button variant="outline" asChild>
              <Link href={c.agent_id ? `/calls/new?agent=${c.agent_id}` : "/calls/new"}>Call again</Link>
            </Button>
          )
        }
      />

      <div className="mb-4 grid grid-cols-2 gap-3 sm:grid-cols-4">
        <Fact label="Length" value={duration(c.duration_secs)} sub={humanize(c.end_reason)} />
        <Fact
          label="Cost (before GST)"
          value={<Money excl={c.cost.total_excl_gst} incl={c.cost.total_incl_gst} gstPercent={c.cost.gst_percent} />}
          sub={c.cost.per_minute_excl_gst !== null ? `${inr(c.cost.per_minute_excl_gst)}/min · hover for incl. GST` : undefined}
        />
        <Fact label="Response time" value={ms(c.latency.voice_to_voice_ms.p50)} sub={`median · p95 ${ms(c.latency.voice_to_voice_ms.p95)}`} />
        <Fact label="Language" value={languageName(c.detected_language ?? c.language)} sub={c.voice ?? undefined} />
      </div>

      <div className="grid gap-4 lg:grid-cols-[1fr_24rem]">
        <Card className="min-w-0">
          <CardHeader>
            <CardTitle>Transcript</CardTitle>
            <CardDescription>
              {live ? "Updates live" : `${c.transcript.filter((t) => t.role !== "tool").length} turns`}
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Transcript c={c} />
          </CardContent>
        </Card>

        <div className="grid content-start gap-4">
          <Recording c={c} />
          <Lead c={c} />
          <CallReviewCard key={`${c.review?.reviewed_at ?? ""}-${c.rating ?? ""}`} c={c} reload={call.reload} />
          {(c.appointments.length > 0 || c.follow_ups.length > 0) && (
            <Card>
              <CardHeader>
                <CardTitle>Bookings & follow-ups</CardTitle>
              </CardHeader>
              <CardContent className="grid gap-2 text-sm">
                {c.appointments.map((a) => (
                  <div key={a.starts_at} className="flex items-center gap-2">
                    <CalendarCheckIcon className="size-4 text-muted-foreground" />
                    <span className="font-medium">{dateTime(a.starts_at)}</span>
                    <span className="text-muted-foreground">
                      {a.name} · {a.duration_mins} min
                    </span>
                    <Badge variant="outline" className="ml-auto capitalize">
                      {a.status}
                    </Badge>
                  </div>
                ))}
                {c.follow_ups.map((f) => (
                  <p key={f.at} className="text-muted-foreground">
                    Send on WhatsApp: <span className="text-foreground">{f.topic}</span>
                  </p>
                ))}
              </CardContent>
            </Card>
          )}
          <Cost c={c} />
        </div>
      </div>

      <Card className="mt-4">
        <CardHeader>
          <CardTitle>Response time per turn</CardTitle>
          <CardDescription>Caller stops speaking → first agent audio sent</CardDescription>
        </CardHeader>
        <CardContent className="grid gap-4 lg:grid-cols-[1fr_20rem]">
          <TurnLatencyChart turns={c.latency.per_turn} />
          <Table>
            <TableBody>
              {[
                ["Voice to voice", c.latency.voice_to_voice_ms],
                ["Speech-to-text final", c.latency.stt_final_ms],
                ["LLM first token", c.latency.llm_first_token_ms],
                ["Voice first byte", c.latency.tts_first_byte_ms],
              ].map(([label, p]) => {
                const v = p as CallDetail["latency"]["stt_final_ms"];
                return (
                  <TableRow key={label as string}>
                    <TableCell>{label as string}</TableCell>
                    <TableCell className="text-right tabular-nums">{ms(v.p50)}</TableCell>
                    <TableCell className="text-right tabular-nums text-muted-foreground">p95 {ms(v.p95)}</TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </CardContent>
      </Card>
    </>
  );
}

function Fact({ label, value, sub }: { label: string; value: React.ReactNode; sub?: string }) {
  return (
    <div className="rounded-xl border bg-card p-3">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className="mt-0.5 text-lg font-semibold tabular-nums">{value}</p>
      {sub && <p className="truncate text-xs text-muted-foreground">{sub}</p>}
    </div>
  );
}

function Transcript({ c }: { c: CallDetail }) {
  const turns = c.transcript.filter((t) => t.role !== "tool");
  if (turns.length === 0) {
    return <p className="py-8 text-center text-sm text-muted-foreground">{c.in_progress ? "Waiting for the first words…" : "No conversation recorded."}</p>;
  }
  return (
    <ol className="grid gap-3">
      {turns.map((t) => {
        const agent = t.role === "assistant";
        return (
          <li key={t.seq} className={cn("flex", agent ? "justify-start" : "justify-end")}>
            <div className={cn("max-w-[85%] rounded-2xl px-3.5 py-2 text-sm", agent ? "rounded-tl-sm bg-muted" : "rounded-tr-sm bg-primary text-primary-foreground")}>
              <p className="whitespace-pre-wrap">
                {t.text}
                {t.interrupted && <span className="ml-1 opacity-60">— interrupted</span>}
              </p>
              <p className={cn("mt-1 text-[11px]", agent ? "text-muted-foreground" : "text-primary-foreground/70")}>
                {agent ? "Agent" : "Caller"} · {time(t.at)}
                {t.language && t.language !== "en-IN" ? ` · ${languageName(t.language)}` : ""}
              </p>
              {t.live_text && t.live_text !== t.text && (
                <p className={cn("mt-1 border-t pt-1 text-[11px]", agent ? "text-muted-foreground" : "border-primary-foreground/20 text-primary-foreground/70")}>
                  Heard live: {t.live_text}
                </p>
              )}
            </div>
          </li>
        );
      })}
    </ol>
  );
}

function Recording({ c }: { c: CallDetail }) {
  // Both links go through the dashboard, which fetches a fresh signed
  // storage link each time, so they keep working however long the page is open.
  const src = `/api/recordings/${c.call_id}`;
  return (
    <Card>
      <CardHeader>
        <CardTitle>Recording</CardTitle>
        {c.recording_url && (
          <CardAction>
            <Button variant="ghost" size="icon-sm" aria-label="Download recording" title="Download recording" asChild>
              <a href={`${src}?download=1`} download>
                <DownloadIcon />
              </a>
            </Button>
          </CardAction>
        )}
      </CardHeader>
      <CardContent>
        {c.recording_url ? (
          <audio controls preload="none" className="w-full" src={src} />
        ) : (
          <p className="text-sm text-muted-foreground">
            {c.answered_at ? (c.in_progress ? "Available after the call." : "Arrives about 20 seconds after the call ends…") : "No recording (call not answered)."}
          </p>
        )}
      </CardContent>
    </Card>
  );
}

function fieldValue(f: LeadField, v: unknown) {
  if (v === null || v === undefined || v === "") return <span className="text-muted-foreground">—</span>;
  if (f.type === "boolean") return v ? "Yes" : "No";
  if (f.type === "number" && typeof v === "number") return v.toLocaleString("en-IN");
  return String(v);
}

function Lead({ c }: { c: CallDetail }) {
  const x = c.extracted_fields;
  if (!x) {
    return (
      <Card>
        <CardHeader>
          <CardTitle>Lead</CardTitle>
        </CardHeader>
        <CardContent>
          <p className="text-sm text-muted-foreground">
            {c.in_progress ? "Extracted when the call ends." : c.answered_at ? "Extracting lead details…" : "Nothing to extract."}
          </p>
        </CardContent>
      </Card>
    );
  }
  const rows: [string, React.ReactNode][] = [
    ["Name", x.name ?? "—"],
    ["Follow-up", x.follow_up_date ?? "—"],
    ["Budget", x.budget ?? "—"],
    ["Language", languageName(x.preferred_language)],
  ];
  return (
    <Card>
      <CardHeader>
        <CardTitle>Lead</CardTitle>
        <CardAction>
          <InterestBadge level={x.interest_level} />
        </CardAction>
      </CardHeader>
      <CardContent className="grid gap-4 text-sm">
        <p>{x.summary}</p>
        {c.collect_fields.length > 0 && (
          <div>
            <p className="mb-1 text-xs font-medium uppercase tracking-wide text-muted-foreground">Collected by the agent</p>
            <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">
              {c.collect_fields.map((f) => (
                <div key={f.key} className="contents">
                  <dt className="text-muted-foreground">{f.label}</dt>
                  <dd className="font-medium">{fieldValue(f, x.custom?.[f.key])}</dd>
                </div>
              ))}
            </dl>
          </div>
        )}
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">
          {rows.map(([k, v]) => (
            <div key={k} className="contents">
              <dt className="text-muted-foreground">{k}</dt>
              <dd>{v}</dd>
            </div>
          ))}
        </dl>
        {x.objections.length > 0 && (
          <div>
            <p className="mb-1 text-xs font-medium uppercase tracking-wide text-muted-foreground">Objections</p>
            <ul className="list-disc space-y-0.5 pl-4">
              {x.objections.map((o) => (
                <li key={o}>{o}</li>
              ))}
            </ul>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function Cost({ c }: { c: CallDetail }) {
  const k = c.cost;
  const rows: [string, number | null, string?][] = [
    ["Speech-to-text", k.ai.stt, `${Math.round(c.usage.stt_seconds)}s audio`],
    ["Re-transcription", k.ai.stt_retranscribe],
    ["Text-to-speech", k.ai.tts, `${compact(c.usage.tts_chars)} chars${k.ai.tts_cached_chars ? `, ${k.ai.tts_cached_chars} cached free` : ""}`],
    ["Greeting audio", k.ai.tts_greeting, k.ai.tts_greeting > 0 ? "new greeting, cached for next calls" : "cached, free"],
    ["LLM", k.ai.llm, k.ai.llm_estimated > 0 ? `incl. ${inr(k.ai.llm_estimated)} estimated for interrupted replies` : undefined],
    ["Lead extraction", k.ai.extraction],
    ["Call review", k.ai.review],
    [
      "Phone call",
      k.telephony.call,
      k.telephony.pending
        ? "waiting for Plivo"
        : k.telephony.billed_secs !== null
          ? `${k.telephony.billed_secs}s billed${k.telephony.unused_paid_secs ? `, ${k.telephony.unused_paid_secs}s unused` : ""}`
          : undefined,
    ],
  ];
  return (
    <Card>
      <CardHeader>
        <CardTitle>Cost</CardTitle>
        <CardDescription>₹, before GST unless noted</CardDescription>
      </CardHeader>
      <CardContent>
        <Table>
          <TableBody>
            {rows.map(([label, v, note]) => (
              <TableRow key={label}>
                <TableCell className="py-1.5">
                  {label}
                  {note && <div className="text-xs text-muted-foreground">{note}</div>}
                </TableCell>
                <TableCell className="py-1.5 text-right tabular-nums">{inr(v, { precise: true })}</TableCell>
              </TableRow>
            ))}
            <TableRow>
              <TableCell className="py-1.5 font-medium">Total</TableCell>
              <TableCell className="py-1.5 text-right font-medium tabular-nums">{inr(k.total_excl_gst, { precise: true })}</TableCell>
            </TableRow>
            <TableRow>
              <TableCell className="py-1.5 font-medium">Incl. {k.gst_percent}% GST</TableCell>
              <TableCell className="py-1.5 text-right font-semibold tabular-nums">{inr(k.total_incl_gst)}</TableCell>
            </TableRow>
          </TableBody>
        </Table>
      </CardContent>
    </Card>
  );
}
