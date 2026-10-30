"use client";

import { Suspense, useMemo, useState } from "react";
import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import { AlertTriangleIcon, BotIcon, PhoneOutgoingIcon } from "lucide-react";
import { toast } from "sonner";

import { ErrorAlert, LoadingBlock, PageHeader } from "@/components/common";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectGroup, SelectItem, SelectLabel, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { useApi } from "@/hooks/use-api";
import { apiSend, errorMessage } from "@/lib/api";
import { humanize, languageName, toE164 } from "@/lib/format";
import type { Agent, KnowledgeBase, Options } from "@/lib/types";

const DEFAULT = "default";

function NewCallForm() {
  const router = useRouter();
  const params = useSearchParams();
  const agents = useApi<{ agents: Agent[] }>("agents");
  const options = useApi<Options>("options", { refreshMs: 60_000 });
  const kbs = useApi<{ knowledge_bases: KnowledgeBase[] }>("knowledge-bases");

  const [picked, setSource] = useState(params.get("agent") ? `agent:${params.get("agent")}` : "");
  const [phone, setPhone] = useState("");
  const [lead, setLead] = useState("");
  const [language, setLanguage] = useState(DEFAULT);
  const [greeting, setGreeting] = useState("");
  const [kb, setKb] = useState(DEFAULT);
  const [booking, setBooking] = useState<boolean | null>(null);
  const [maxSecs, setMaxSecs] = useState("");
  const [extra, setExtra] = useState("");
  const [busy, setBusy] = useState(false);

  // Until something is picked, default to the first agent (or persona).
  const source =
    picked ||
    (agents.data?.agents[0] ? `agent:${agents.data.agents[0].id}` : options.data?.personas[0] ? `persona:${options.data.personas[0].name}` : "");

  const agent = useMemo(
    () => (source.startsWith("agent:") ? agents.data?.agents.find((a) => a.id === source.slice(6)) : undefined),
    [source, agents.data],
  );
  const e164 = toE164(phone);
  const phoneOk = /^\+[1-9]\d{7,14}$/.test(e164);
  const maxGreeting = options.data?.greeting_max_chars ?? 100;
  const hours = options.data?.call_hours;

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!phoneOk || !source) return;
    setBusy(true);
    const body: Record<string, unknown> = { to: e164, lead_name: lead.trim() || "there" };
    if (source.startsWith("agent:")) body.agent_id = source.slice(6);
    else body.persona = source.slice(8);
    if (language !== DEFAULT) body.language = language;
    if (greeting.trim()) body.greeting = greeting.trim();
    if (kb !== DEFAULT) body.knowledge_base_id = kb;
    if (booking !== null) body.booking = booking;
    if (maxSecs) body.max_duration_secs = Number(maxSecs);
    if (extra.trim()) body.system_prompt = extra.trim();
    try {
      const res = await apiSend<{ call_id: string }>("POST", "calls", body);
      toast.success("Calling…", { description: `${e164.slice(0, -4).replace(/\d/g, "•")}${e164.slice(-4)}` });
      router.push(`/calls/${res.call_id}`);
    } catch (err) {
      toast.error("Call not placed", { description: errorMessage(err) });
      setBusy(false);
    }
  }

  if (agents.loading || options.loading) return <LoadingBlock rows={4} />;
  if (options.error) return <ErrorAlert error={options.error} title="Could not reach the voice backend" />;
  const o = options.data!;
  const bookingDefault = agent ? agent.booking : false;

  return (
    <form onSubmit={submit} className="grid gap-4 lg:grid-cols-[1fr_22rem]">
      <div className="grid gap-4">
        {hours && !hours.open_now && (
          <Alert>
            <AlertTriangleIcon />
            <AlertTitle>Outside calling hours</AlertTitle>
            <AlertDescription>
              Calls are allowed only between {hours.start_hour}:00 and {hours.end_hour}:00 ({hours.timezone}). The call will be refused.
            </AlertDescription>
          </Alert>
        )}
        <Card>
          <CardHeader>
            <CardTitle>Who to call</CardTitle>
          </CardHeader>
          <CardContent className="grid gap-4 sm:grid-cols-2">
            <div className="grid gap-2">
              <Label htmlFor="phone">Phone number</Label>
              <Input
                id="phone"
                inputMode="tel"
                placeholder="98765 43210 or +14155550123"
                value={phone}
                onChange={(e) => setPhone(e.target.value)}
                aria-invalid={phone !== "" && !phoneOk}
                required
              />
              <p className="text-xs text-muted-foreground">
                {phone && phoneOk ? `Dials ${e164}` : "10-digit numbers get +91. Use E.164 for other countries."}
              </p>
            </div>
            <div className="grid gap-2">
              <Label htmlFor="lead">Lead name</Label>
              <Input id="lead" placeholder="Ravi Kumar" maxLength={100} value={lead} onChange={(e) => setLead(e.target.value)} />
              <p className="text-xs text-muted-foreground">Leave empty if unknown; the agent will ask.</p>
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Agent</CardTitle>
            <CardDescription>The character, the details it collects and its knowledge base.</CardDescription>
          </CardHeader>
          <CardContent className="grid gap-4">
            <Select value={source} onValueChange={setSource}>
              <SelectTrigger className="w-full" aria-label="Agent">
                <SelectValue placeholder="Choose an agent" />
              </SelectTrigger>
              <SelectContent>
                {agents.data && agents.data.agents.length > 0 && (
                  <SelectGroup>
                    <SelectLabel>Your agents</SelectLabel>
                    {agents.data.agents.map((a) => (
                      <SelectItem key={a.id} value={`agent:${a.id}`}>
                        {a.name}
                      </SelectItem>
                    ))}
                  </SelectGroup>
                )}
                <SelectGroup>
                  <SelectLabel>Built-in personas</SelectLabel>
                  {o.personas.map((p) => (
                    <SelectItem key={p.name} value={`persona:${p.name}`}>
                      {humanize(p.name)}
                    </SelectItem>
                  ))}
                </SelectGroup>
              </SelectContent>
            </Select>
            {agent ? (
              <div className="rounded-lg border bg-muted/30 p-3 text-sm">
                <p className="line-clamp-3 text-muted-foreground">{agent.description || agent.prompt}</p>
                <p className="mt-2 text-xs text-muted-foreground">
                  {languageName(agent.language)} · {agent.fields.length} field{agent.fields.length === 1 ? "" : "s"} to collect
                  {agent.knowledge_base_id ? " · knowledge base" : ""}
                  {agent.booking ? " · booking on" : ""}
                </p>
                <Link href={`/agents/${agent.id}`} className="mt-2 inline-block text-xs underline">
                  Edit agent
                </Link>
              </div>
            ) : (
              agents.data?.agents.length === 0 && (
                <p className="text-sm text-muted-foreground">
                  <BotIcon className="mr-1 inline size-4" />
                  <Link className="underline" href="/agents/new">
                    Create an agent
                  </Link>{" "}
                  to set your own character, fields and knowledge base.
                </p>
              )
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>This call only</CardTitle>
            <CardDescription>Optional. Anything left blank uses the agent&apos;s settings.</CardDescription>
          </CardHeader>
          <CardContent className="grid gap-4 sm:grid-cols-2">
            <div className="grid gap-2">
              <Label>Language</Label>
              <Select value={language} onValueChange={setLanguage}>
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value={DEFAULT}>Agent default{agent ? ` (${languageName(agent.language)})` : ""}</SelectItem>
                  <SelectItem value="auto">Auto-detect</SelectItem>
                  {o.languages.map((l) => (
                    <SelectItem key={l.code} value={l.code}>
                      {languageName(l.code)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="grid gap-2">
              <Label>Knowledge base</Label>
              <Select value={kb} onValueChange={setKb}>
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value={DEFAULT}>Agent default</SelectItem>
                  {kbs.data?.knowledge_bases.map((k) => (
                    <SelectItem key={k.id} value={k.id}>
                      {k.name} ({k.ready_documents} docs)
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="grid gap-2 sm:col-span-2">
              <div className="flex items-center justify-between">
                <Label htmlFor="greeting">Greeting</Label>
                <span className={`text-xs tabular-nums ${greeting.length > maxGreeting ? "text-destructive" : "text-muted-foreground"}`}>
                  {greeting.length}/{maxGreeting}
                </span>
              </div>
              <Input
                id="greeting"
                placeholder={agent?.greeting ?? "I'm calling from … about … Is now a good time?"}
                value={greeting}
                onChange={(e) => setGreeting(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">Played after the fixed line that says this is an AI assistant and the call is recorded.</p>
            </div>
            <div className="grid gap-2">
              <Label htmlFor="max">Max length (seconds)</Label>
              <Input
                id="max"
                type="number"
                min={10}
                max={3600}
                placeholder={String(agent?.max_duration_secs ?? o.default_max_duration_secs)}
                value={maxSecs}
                onChange={(e) => setMaxSecs(e.target.value)}
              />
            </div>
            <div className="flex items-center justify-between gap-4 rounded-lg border p-3">
              <div>
                <Label htmlFor="booking">Appointment booking</Label>
                <p className="text-xs text-muted-foreground">Agent default: {bookingDefault ? "on" : "off"}</p>
              </div>
              <Switch id="booking" checked={booking ?? bookingDefault} onCheckedChange={setBooking} />
            </div>
            <div className="grid gap-2 sm:col-span-2">
              <Label htmlFor="extra">Extra instructions</Label>
              <Textarea
                id="extra"
                rows={3}
                placeholder="Facts about this lead, e.g. They visited the site last week and asked about 3BHK prices."
                value={extra}
                onChange={(e) => setExtra(e.target.value)}
              />
            </div>
          </CardContent>
        </Card>
      </div>

      <div className="lg:sticky lg:top-20 lg:self-start">
        <Card>
          <CardHeader>
            <CardTitle>Ready to dial</CardTitle>
            <CardDescription>The call starts right away. You can follow it live and hang up from the next page.</CardDescription>
          </CardHeader>
          <CardContent className="grid gap-3">
            <Button type="submit" size="lg" disabled={busy || !phoneOk || !source || greeting.length > maxGreeting}>
              <PhoneOutgoingIcon data-icon="inline-start" />
              {busy ? "Placing call…" : "Place call"}
            </Button>
            <p className="text-xs text-muted-foreground">
              Calls are recorded and the agent says so first. Allowed {hours?.start_hour}:00–{hours?.end_hour}:00 {hours?.timezone}.
            </p>
          </CardContent>
        </Card>
      </div>
    </form>
  );
}

export default function NewCallPage() {
  return (
    <>
      <PageHeader title="New call" description="Place an outbound AI call." />
      <Suspense fallback={<LoadingBlock rows={4} />}>
        <NewCallForm />
      </Suspense>
    </>
  );
}
