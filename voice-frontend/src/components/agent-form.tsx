"use client";

import { useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { ArrowDownIcon, ArrowUpIcon, PlusIcon, SaveIcon, Trash2Icon, WandSparklesIcon } from "lucide-react";
import { toast } from "sonner";

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
import { PromptAssistant } from "@/components/prompt-assistant";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { useApi } from "@/hooks/use-api";
import { apiSend, errorMessage } from "@/lib/api";
import { humanize, languageName, toKey } from "@/lib/format";
import type { Agent, FieldKind, KnowledgeBase, LeadField, Options } from "@/lib/types";

const NONE = "none";

interface Draft {
  name: string;
  description: string;
  prompt: string;
  greeting: string;
  knowledge_base_id: string;
  language: string;
  booking: boolean;
  max_duration_secs: string;
  fields: (LeadField & { optionsText: string; keyEdited: boolean })[];
}

function toDraft(a?: Agent): Draft {
  return {
    name: a?.name ?? "",
    description: a?.description ?? "",
    prompt: a?.prompt ?? "",
    greeting: a?.greeting ?? "",
    knowledge_base_id: a?.knowledge_base_id ?? NONE,
    language: a?.language ?? "auto",
    booking: a?.booking ?? false,
    max_duration_secs: a?.max_duration_secs ? String(a.max_duration_secs) : "",
    fields: (a?.fields ?? []).map((f) => ({ ...f, optionsText: (f.options ?? []).join(", "), keyEdited: true })),
  };
}

// Starting points for the fields list; editable after adding.
const FIELD_PRESETS: { label: string; fields: Omit<LeadField, "required">[] }[] = [
  {
    label: "Real estate",
    fields: [
      { key: "city", label: "City / area", description: "Where they want to buy", type: "text" },
      { key: "bhk", label: "BHK", description: "Flat size wanted", type: "choice", options: ["1", "2", "3", "4+"] },
      { key: "budget_lakh", label: "Budget (lakh ₹)", description: "Upper limit in lakh rupees", type: "number" },
      { key: "move_in", label: "Move-in time", description: "When they plan to buy or move", type: "text" },
      { key: "site_visit", label: "Wants a site visit", description: "", type: "boolean" },
    ],
  },
  {
    label: "Course admissions",
    fields: [
      { key: "course", label: "Course interested in", description: "", type: "text" },
      { key: "qualification", label: "Highest qualification", description: "", type: "text" },
      { key: "mode", label: "Mode", description: "", type: "choice", options: ["Online", "Offline", "Hybrid"] },
      { key: "start_date", label: "Preferred start date", description: "", type: "date" },
    ],
  },
  {
    label: "Insurance",
    fields: [
      { key: "age", label: "Age", description: "", type: "number" },
      { key: "existing_policy", label: "Has a policy already", description: "", type: "boolean" },
      { key: "cover_lakh", label: "Cover wanted (lakh ₹)", description: "", type: "number" },
      { key: "dependents", label: "Dependents", description: "Spouse, children, parents", type: "text" },
    ],
  },
];

export function AgentForm({ agent }: { agent?: Agent }) {
  const router = useRouter();
  const options = useApi<Options>("options");
  const kbs = useApi<{ knowledge_bases: KnowledgeBase[] }>("knowledge-bases");
  const [d, setD] = useState<Draft>(() => toDraft(agent));
  const [busy, setBusy] = useState(false);
  const maxGreeting = options.data?.greeting_max_chars ?? 100;

  const set = <K extends keyof Draft>(k: K, v: Draft[K]) => setD((x) => ({ ...x, [k]: v }));
  const setField = (i: number, patch: Partial<Draft["fields"][number]>) =>
    setD((x) => ({ ...x, fields: x.fields.map((f, j) => (j === i ? { ...f, ...patch } : f)) }));
  const addFields = (fields: Omit<LeadField, "required">[]) =>
    setD((x) => {
      const keys = new Set(x.fields.map((f) => f.key));
      const fresh = fields
        .filter((f) => !keys.has(f.key))
        .map((f) => ({ ...f, required: false, optionsText: (f.options ?? []).join(", "), keyEdited: true }));
      return { ...x, fields: [...x.fields, ...fresh] };
    });
  const move = (i: number, by: number) =>
    setD((x) => {
      const f = [...x.fields];
      const [item] = f.splice(i, 1);
      f.splice(Math.max(0, Math.min(f.length, i + by)), 0, item);
      return { ...x, fields: f };
    });

  async function save(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    const body = {
      name: d.name,
      description: d.description,
      prompt: d.prompt,
      greeting: d.greeting.trim() || null,
      knowledge_base_id: d.knowledge_base_id === NONE ? null : d.knowledge_base_id,
      language: d.language,
      booking: d.booking,
      max_duration_secs: d.max_duration_secs ? Number(d.max_duration_secs) : null,
      fields: d.fields.map((f) => ({
        key: f.key,
        label: f.label,
        description: f.description,
        type: f.type,
        required: f.required,
        ...(f.type === "choice" ? { options: f.optionsText.split(",").map((o) => o.trim()).filter(Boolean) } : {}),
      })),
    };
    try {
      const saved = agent ? await apiSend<Agent>("PUT", `agents/${agent.id}`, body) : await apiSend<Agent>("POST", "agents", body);
      toast.success(agent ? "Agent saved" : "Agent created");
      if (agent) setD(toDraft(saved));
      else router.push(`/agents/${saved.id}`);
    } catch (err) {
      toast.error("Not saved", { description: errorMessage(err) });
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    if (!agent) return;
    try {
      await apiSend("DELETE", `agents/${agent.id}`);
      toast.success("Agent deleted");
      router.push("/agents");
    } catch (err) {
      toast.error("Not deleted", { description: errorMessage(err) });
    }
  }

  const o = options.data;
  return (
    <form onSubmit={save} className="grid gap-4">
      <Card>
        <CardHeader>
          <CardTitle>Character</CardTitle>
          <CardDescription>Who the agent is, what the call is for and how it should handle common questions.</CardDescription>
          {o && o.personas.length > 0 && (
            <CardAction>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button type="button" variant="outline" size="sm">
                    <WandSparklesIcon data-icon="inline-start" />
                    Start from a persona
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end">
                  <DropdownMenuLabel>Replaces the prompt</DropdownMenuLabel>
                  {o.personas.map((p) => (
                    <DropdownMenuItem key={p.name} onSelect={() => set("prompt", p.prompt)}>
                      {humanize(p.name)}
                    </DropdownMenuItem>
                  ))}
                </DropdownMenuContent>
              </DropdownMenu>
            </CardAction>
          )}
        </CardHeader>
        <CardContent className="grid gap-4 md:grid-cols-2">
          <div className="grid gap-2">
            <Label htmlFor="name">Name</Label>
            <Input id="name" required maxLength={100} placeholder="Star Homes – flat enquiries" value={d.name} onChange={(e) => set("name", e.target.value)} />
          </div>
          <div className="grid gap-2">
            <Label htmlFor="description">Note (for your team)</Label>
            <Input id="description" maxLength={500} placeholder="Calls website leads for 2–3BHK projects" value={d.description} onChange={(e) => set("description", e.target.value)} />
          </div>
          <div className="grid gap-2 md:col-span-2">
            <div className="flex items-center justify-between">
              <Label htmlFor="prompt">Prompt</Label>
              <span className="text-xs tabular-nums text-muted-foreground">{d.prompt.length.toLocaleString()} chars</span>
            </div>
            <Textarea
              id="prompt"
              required
              className="min-h-64 font-mono text-[13px]"
              placeholder={
                "You are Priya, a friendly caller for Star Homes in Mysuru.\nGoal: find out if the lead still wants a flat and book a site visit.\nFacts: projects in Vijayanagar and Hebbal, 2BHK from ₹48 lakh…\nIf asked about loans: we partner with SBI and HDFC."
              }
              value={d.prompt}
              onChange={(e) => set("prompt", e.target.value)}
            />
            <p className="text-xs text-muted-foreground">
              Speaking rules (short replies, no URLs read aloud, the AI disclosure) are added automatically. Put long product details in a knowledge base instead.
            </p>
            <PromptAssistant prompt={d.prompt} onApply={(p) => set("prompt", p)} />
          </div>
          <div className="grid gap-2 md:col-span-2">
            <div className="flex items-center justify-between">
              <Label htmlFor="greeting">Greeting</Label>
              <span className={`text-xs tabular-nums ${d.greeting.length > maxGreeting ? "text-destructive" : "text-muted-foreground"}`}>
                {d.greeting.length}/{maxGreeting}
              </span>
            </div>
            <Input
              id="greeting"
              placeholder="I'm calling from Star Homes about the flat you enquired about. Is now a good time?"
              value={d.greeting}
              onChange={(e) => set("greeting", e.target.value)}
            />
            <p className="text-xs text-muted-foreground">Follows the fixed opening line: &ldquo;Hi, I&apos;m an AI assistant, and this call is recorded.&rdquo;</p>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Details to collect</CardTitle>
          <CardDescription>
            The agent asks for these during the call, and they are filled in on the call page afterwards. Name, interest, follow-up date and a summary are always captured.
          </CardDescription>
          <CardAction>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button type="button" variant="outline" size="sm">
                  Presets
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                {FIELD_PRESETS.map((p) => (
                  <DropdownMenuItem key={p.label} onSelect={() => addFields(p.fields)}>
                    {p.label}
                  </DropdownMenuItem>
                ))}
              </DropdownMenuContent>
            </DropdownMenu>
          </CardAction>
        </CardHeader>
        <CardContent className="grid gap-3">
          {d.fields.length === 0 && <p className="text-sm text-muted-foreground">No extra details. Add a field or pick a preset.</p>}
          {d.fields.map((f, i) => (
            <div key={i} className="grid gap-3 rounded-lg border p-3 md:grid-cols-[1.2fr_9rem_1.5fr_auto]">
              <div className="grid gap-1.5">
                <Label className="text-xs text-muted-foreground">Label</Label>
                <Input
                  required
                  maxLength={80}
                  placeholder="City"
                  value={f.label}
                  onChange={(e) => setField(i, { label: e.target.value, ...(f.keyEdited ? {} : { key: toKey(e.target.value) }) })}
                />
                <Input
                  aria-label="Key"
                  className="h-7 font-mono text-xs"
                  placeholder="key"
                  value={f.key}
                  pattern="[a-z][a-z0-9_]{0,39}"
                  title="lowercase letters, digits and _; starts with a letter"
                  onChange={(e) => setField(i, { key: e.target.value, keyEdited: true })}
                />
              </div>
              <div className="grid content-start gap-1.5">
                <Label className="text-xs text-muted-foreground">Type</Label>
                <Select value={f.type} onValueChange={(v) => setField(i, { type: v as FieldKind })}>
                  <SelectTrigger className="w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="text">Text</SelectItem>
                    <SelectItem value="number">Number</SelectItem>
                    <SelectItem value="date">Date</SelectItem>
                    <SelectItem value="boolean">Yes / no</SelectItem>
                    <SelectItem value="choice">Choice</SelectItem>
                  </SelectContent>
                </Select>
                <label className="flex items-center gap-2 text-xs">
                  <Checkbox checked={f.required} onCheckedChange={(v) => setField(i, { required: v === true })} />
                  Required
                </label>
              </div>
              <div className="grid content-start gap-1.5">
                <Label className="text-xs text-muted-foreground">{f.type === "choice" ? "Options (comma separated)" : "Hint for the agent"}</Label>
                {f.type === "choice" ? (
                  <Input required placeholder="1, 2, 3" value={f.optionsText} onChange={(e) => setField(i, { optionsText: e.target.value })} />
                ) : null}
                <Input
                  maxLength={300}
                  placeholder={f.type === "choice" ? "Hint (optional)" : "What to ask, e.g. which area they prefer"}
                  value={f.description}
                  onChange={(e) => setField(i, { description: e.target.value })}
                />
              </div>
              <div className="flex items-start gap-1 md:flex-col">
                <Button type="button" variant="ghost" size="icon-sm" aria-label="Move up" disabled={i === 0} onClick={() => move(i, -1)}>
                  <ArrowUpIcon />
                </Button>
                <Button type="button" variant="ghost" size="icon-sm" aria-label="Move down" disabled={i === d.fields.length - 1} onClick={() => move(i, 1)}>
                  <ArrowDownIcon />
                </Button>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-sm"
                  aria-label="Remove field"
                  onClick={() => setD((x) => ({ ...x, fields: x.fields.filter((_, j) => j !== i) }))}
                >
                  <Trash2Icon />
                </Button>
              </div>
            </div>
          ))}
          <div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={d.fields.length >= 20}
              onClick={() =>
                setD((x) => ({
                  ...x,
                  fields: [...x.fields, { key: "", label: "", description: "", type: "text", required: false, optionsText: "", keyEdited: false }],
                }))
              }
            >
              <PlusIcon data-icon="inline-start" />
              Add field
            </Button>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Knowledge & defaults</CardTitle>
          <CardDescription>Each call can still override these.</CardDescription>
        </CardHeader>
        <CardContent className="grid items-start gap-4 md:grid-cols-2">
          <div className="grid gap-2">
            <Label>Knowledge base</Label>
            <Select value={d.knowledge_base_id} onValueChange={(v) => set("knowledge_base_id", v)}>
              <SelectTrigger className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value={NONE}>None</SelectItem>
                {kbs.data?.knowledge_bases.map((k) => (
                  <SelectItem key={k.id} value={k.id}>
                    {k.name} ({k.ready_documents} docs, {k.chunks} chunks)
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <p className="text-xs text-muted-foreground">
              The agent searches it when the caller asks something. <Link className="underline" href="/knowledge">Manage knowledge bases</Link>
            </p>
          </div>
          <div className="grid gap-2">
            <Label>Language</Label>
            <Select value={d.language} onValueChange={(v) => set("language", v)}>
              <SelectTrigger className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="auto">Auto-detect (switches with the caller)</SelectItem>
                {o?.languages.map((l) => (
                  <SelectItem key={l.code} value={l.code}>
                    {languageName(l.code)} · {l.speaker.split("_")[0]}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="grid gap-2">
            <Label htmlFor="max">Max call length (seconds)</Label>
            <Input
              id="max"
              type="number"
              min={10}
              max={3600}
              placeholder={String(o?.default_max_duration_secs ?? 300)}
              value={d.max_duration_secs}
              onChange={(e) => set("max_duration_secs", e.target.value)}
            />
          </div>
          <div className="flex items-center justify-between gap-4 rounded-lg border p-3">
            <div>
              <Label htmlFor="booking">Appointment booking</Label>
              <p className="text-xs text-muted-foreground">Checks free slots and books them during the call.</p>
            </div>
            <Switch id="booking" checked={d.booking} onCheckedChange={(v) => set("booking", v)} />
          </div>
        </CardContent>
      </Card>

      <div className="sticky bottom-0 -mx-4 flex flex-wrap items-center gap-2 border-t bg-background/95 px-4 py-3 backdrop-blur md:-mx-6 md:px-6">
        <Button type="submit" disabled={busy || d.greeting.length > maxGreeting}>
          <SaveIcon data-icon="inline-start" />
          {busy ? "Saving…" : agent ? "Save changes" : "Create agent"}
        </Button>
        {agent && (
          <Button type="button" variant="outline" asChild>
            <Link href={`/calls/new?agent=${agent.id}`}>Place a call</Link>
          </Button>
        )}
        <div className="flex-1" />
        {agent && (
          <AlertDialog>
            <AlertDialogTrigger asChild>
              <Button type="button" variant="ghost" className="text-destructive">
                <Trash2Icon data-icon="inline-start" />
                Delete
              </Button>
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>Delete {agent.name}?</AlertDialogTitle>
                <AlertDialogDescription>Past calls keep their transcripts and collected details.</AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>Cancel</AlertDialogCancel>
                <AlertDialogAction onClick={remove}>Delete</AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        )}
      </div>
    </form>
  );
}
