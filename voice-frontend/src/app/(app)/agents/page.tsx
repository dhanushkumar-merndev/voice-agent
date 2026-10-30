"use client";

import Link from "next/link";
import { BookOpenIcon, BotIcon, CalendarCheckIcon, GraduationCapIcon, ListChecksIcon, PackageIcon, PhoneOutgoingIcon, PlusIcon } from "lucide-react";

import { EmptyState, ErrorAlert, LoadingBlock, PageHeader } from "@/components/common";
import { ScoreBadge } from "@/components/training";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
import { useApi } from "@/hooks/use-api";
import { languageName, relative } from "@/lib/format";
import type { Agent, KnowledgeBase } from "@/lib/types";

export default function AgentsPage() {
  const agents = useApi<{ agents: Agent[] }>("agents");
  const kbs = useApi<{ knowledge_bases: KnowledgeBase[] }>("knowledge-bases");
  const kbName = (id: string | null) => kbs.data?.knowledge_bases.find((k) => k.id === id)?.name;

  return (
    <>
      <PageHeader
        title="Agents"
        description="Each agent is a character: what it says, which details it collects and which knowledge base it answers from. One voice per language is shared by all agents."
        actions={
          <>
            <Button asChild variant="outline">
              <Link href="/agents/import">
                <PackageIcon data-icon="inline-start" />
                Import pack
              </Link>
            </Button>
            <Button asChild>
              <Link href="/agents/new">
              <PlusIcon data-icon="inline-start" />
                New agent
              </Link>
            </Button>
          </>
        }
      />
      {agents.error && !agents.data && <ErrorAlert error={agents.error} title="Could not load agents" />}
      {agents.loading && <LoadingBlock rows={3} />}
      {agents.data?.agents.length === 0 && (
        <EmptyState icon={BotIcon} title="No agents yet">
          Create one for each kind of call: a clinic receptionist, a real-estate caller, a course counsellor…
          <div className="mt-3">
            <Button asChild size="sm">
              <Link href="/agents/new">Create your first agent</Link>
            </Button>
          </div>
        </EmptyState>
      )}
      <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
        {agents.data?.agents.map((a) => (
          <Card key={a.id} className="flex flex-col">
            <CardHeader>
              <CardTitle>
                <Link href={`/agents/${a.id}`} className="hover:underline">
                  {a.name}
                </Link>
              </CardTitle>
              <CardDescription className="line-clamp-2">{a.description || a.prompt}</CardDescription>
            </CardHeader>
            <CardContent className="flex flex-1 flex-wrap content-start gap-1.5">
              <Badge variant="secondary">{languageName(a.language)}</Badge>
              <Badge variant="secondary">
                <ListChecksIcon />
                {a.fields.length} field{a.fields.length === 1 ? "" : "s"}
              </Badge>
              {a.knowledge_base_id && (
                <Badge variant="secondary">
                  <BookOpenIcon />
                  {kbName(a.knowledge_base_id) ?? "Knowledge base"}
                </Badge>
              )}
              {a.booking && (
                <Badge variant="secondary">
                  <CalendarCheckIcon />
                  Booking
                </Badge>
              )}
              <ScoreBadge score={a.avg_review_score} label="Avg review" />
              {(a.suggested_lessons ?? 0) > 0 && (
                <Badge asChild variant="outline" className="border-amber-500/40 text-amber-700 dark:text-amber-300">
                  <Link href={`/agents/${a.id}#training`}>
                    <GraduationCapIcon />
                    {a.suggested_lessons} lesson{a.suggested_lessons === 1 ? "" : "s"} to review
                  </Link>
                </Badge>
              )}
            </CardContent>
            <CardFooter className="justify-between gap-2 text-xs text-muted-foreground">
              <span>
                {a.calls ?? 0} call{a.calls === 1 ? "" : "s"}
                {a.last_call_at ? ` · last ${relative(a.last_call_at)}` : ""}
              </span>
              <div className="flex gap-2">
                <Button asChild variant="outline" size="sm">
                  <Link href={`/agents/${a.id}`}>Edit</Link>
                </Button>
                <Button asChild size="sm">
                  <Link href={`/calls/new?agent=${a.id}`}>
                    <PhoneOutgoingIcon data-icon="inline-start" />
                    Call
                  </Link>
                </Button>
              </div>
            </CardFooter>
          </Card>
        ))}
      </div>
    </>
  );
}
