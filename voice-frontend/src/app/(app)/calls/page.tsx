"use client";

import { useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { ChevronLeftIcon, ChevronRightIcon, PhoneCallIcon } from "lucide-react";

import { EmptyState, ErrorAlert, InterestBadge, LoadingBlock, Money, PageHeader, StatusBadge } from "@/components/common";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { useApi } from "@/hooks/use-api";
import { dateTime, duration, humanize, languageName, ms } from "@/lib/format";
import type { Agent, CallList } from "@/lib/types";

const PAGE = 25;
const STATUSES = ["completed", "in_progress", "ringing", "queued", "no_answer", "busy", "failed"];

export default function CallsPage() {
  const router = useRouter();
  const [status, setStatus] = useState("all");
  const [agent, setAgent] = useState("all");
  const [offset, setOffset] = useState(0);
  const query = new URLSearchParams({ limit: String(PAGE), offset: String(offset) });
  if (status !== "all") query.set("status", status);
  if (agent !== "all") query.set("agent_id", agent);
  const calls = useApi<CallList>(`calls?${query}`, {
    refreshMs: (d) => (d?.calls.some((c) => c.in_progress) ? 3000 : 20_000),
  });
  const agents = useApi<{ agents: Agent[] }>("agents");
  const total = calls.data?.total ?? 0;

  return (
    <>
      <PageHeader
        title="Calls"
        description={calls.data ? `${total} call${total === 1 ? "" : "s"}` : undefined}
        actions={
          <>
            <Select
              value={agent}
              onValueChange={(v) => {
                setAgent(v);
                setOffset(0);
              }}
            >
              <SelectTrigger className="w-44" aria-label="Agent">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All agents</SelectItem>
                {agents.data?.agents.map((a) => (
                  <SelectItem key={a.id} value={a.id}>
                    {a.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Select
              value={status}
              onValueChange={(v) => {
                setStatus(v);
                setOffset(0);
              }}
            >
              <SelectTrigger className="w-40" aria-label="Status">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="all">All statuses</SelectItem>
                {STATUSES.map((s) => (
                  <SelectItem key={s} value={s}>
                    {humanize(s)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </>
        }
      />
      {calls.error && !calls.data && <ErrorAlert error={calls.error} title="Could not load calls" />}
      {calls.loading && <LoadingBlock rows={6} />}
      {calls.data && calls.data.calls.length === 0 && (
        <EmptyState icon={PhoneCallIcon} title="No calls match">
          <Link className="underline" href="/calls/new">
            Place a call
          </Link>
        </EmptyState>
      )}
      {calls.data && calls.data.calls.length > 0 && (
        <Card className="py-0">
          <CardContent className="px-0">
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead className="pl-4">Status</TableHead>
                  <TableHead>Lead</TableHead>
                  <TableHead className="hidden md:table-cell">Agent</TableHead>
                  <TableHead className="hidden lg:table-cell">Language</TableHead>
                  <TableHead>Interest</TableHead>
                  <TableHead className="text-right">Length</TableHead>
                  <TableHead className="hidden text-right sm:table-cell">Response</TableHead>
                  <TableHead className="text-right">Cost (before GST)</TableHead>
                  <TableHead className="hidden pr-4 text-right md:table-cell">Placed</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {calls.data.calls.map((c) => (
                  <TableRow key={c.call_id} className="cursor-pointer" onClick={() => router.push(`/calls/${c.call_id}`)}>
                    <TableCell className="pl-4">
                      <StatusBadge status={c.status} live={c.in_progress} />
                    </TableCell>
                    <TableCell>
                      <Link href={`/calls/${c.call_id}`} className="font-medium hover:underline" onClick={(e) => e.stopPropagation()}>
                        {c.lead_name}
                      </Link>
                      <div className="text-xs text-muted-foreground tabular-nums">{c.to}</div>
                    </TableCell>
                    <TableCell className="hidden md:table-cell">{c.agent_name ?? <span className="text-muted-foreground">—</span>}</TableCell>
                    <TableCell className="hidden lg:table-cell">{languageName(c.detected_language ?? c.language)}</TableCell>
                    <TableCell>
                      <InterestBadge level={c.interest_level} />
                    </TableCell>
                    <TableCell className="text-right tabular-nums">{duration(c.duration_secs)}</TableCell>
                    <TableCell className="hidden text-right tabular-nums sm:table-cell">{ms(c.voice_to_voice_p50_ms)}</TableCell>
                    <TableCell className="text-right tabular-nums">
                      <Money excl={c.cost_excl_gst} incl={c.cost_incl_gst} />
                      {c.telephony_pending && <div className="text-xs text-muted-foreground">phone pending</div>}
                    </TableCell>
                    <TableCell className="hidden pr-4 text-right text-sm text-muted-foreground md:table-cell">{dateTime(c.created_at)}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </CardContent>
        </Card>
      )}
      {total > PAGE && (
        <div className="mt-4 flex items-center justify-end gap-2 text-sm">
          <span className="text-muted-foreground">
            {offset + 1}–{Math.min(offset + PAGE, total)} of {total}
          </span>
          <Button variant="outline" size="icon" aria-label="Previous page" disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - PAGE))}>
            <ChevronLeftIcon />
          </Button>
          <Button variant="outline" size="icon" aria-label="Next page" disabled={offset + PAGE >= total} onClick={() => setOffset(offset + PAGE)}>
            <ChevronRightIcon />
          </Button>
        </div>
      )}
    </>
  );
}
