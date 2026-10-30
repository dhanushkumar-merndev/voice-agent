"use client";

import { useRef, useState } from "react";
import { useRouter } from "next/navigation";
import {
  AlertTriangleIcon,
  CheckCircle2Icon,
  CheckIcon,
  CircleIcon,
  CopyIcon,
  DownloadIcon,
  FileArchiveIcon,
  FileTextIcon,
  Loader2Icon,
  PackageIcon,
  ShieldCheckIcon,
  XCircleIcon,
} from "lucide-react";
import { toast } from "sonner";
import { strToU8 } from "fflate";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
import { Progress } from "@/components/ui/progress";
import { Textarea } from "@/components/ui/textarea";
import { ApiError, apiGet, apiSend, apiUpload, errorMessage } from "@/lib/api";
import { PACK_PROMPT, buildPack, packFileName, parsePack, type PackAgent, type ParsedPack } from "@/lib/agent-pack";
import { bytes as formatBytes, languageName } from "@/lib/format";
import type { Agent, KbDocument, KbDocumentDetail, Training } from "@/lib/types";
import { cn } from "@/lib/utils";

/** How long to wait for uploaded files to be indexed. */
const INDEX_TIMEOUT_MS = 10 * 60 * 1000;

type StepState = "waiting" | "running" | "done" | "failed";
interface Step {
  id: string;
  label: string;
  state: StepState;
  detail?: string;
  progress?: number;
}

/** Copy the prompt, upload the ZIP the AI made, check it, then create the agent. */
export function ImportPack() {
  const router = useRouter();
  const input = useRef<HTMLInputElement>(null);
  const [copied, setCopied] = useState(false);
  const [fileName, setFileName] = useState<string | null>(null);
  const [pack, setPack] = useState<ParsedPack | null>(null);
  const [dragging, setDragging] = useState(false);
  const [steps, setSteps] = useState<Step[] | null>(null);
  const [busy, setBusy] = useState(false);

  async function copyPrompt() {
    try {
      await navigator.clipboard.writeText(PACK_PROMPT);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      toast.error("Could not copy. Select the text and copy it yourself.");
    }
  }

  async function load(file: File | undefined) {
    if (!file || busy) return;
    setSteps(null);
    setFileName(file.name);
    if (!/\.zip$/i.test(file.name)) {
      setPack({ agent: null, files: [], training: null, errors: ["Choose the .zip file the AI made."], warnings: [] });
      return;
    }
    setPack(parsePack(new Uint8Array(await file.arrayBuffer())));
  }

  function update(id: string, patch: Partial<Step>) {
    setSteps((s) => s?.map((x) => (x.id === id ? { ...x, ...patch } : x)) ?? null);
  }

  async function create() {
    if (!pack?.agent || pack.errors.length > 0) return;
    const agentIn = pack.agent;
    const plan: Step[] = [];
    if (pack.files.length > 0) {
      plan.push({ id: "kb", label: "Create the knowledge base", state: "waiting" });
      plan.push({ id: "upload", label: `Upload ${pack.files.length} file${pack.files.length === 1 ? "" : "s"}`, state: "waiting" });
      plan.push({ id: "index", label: "Read and index the files", state: "waiting" });
    }
    plan.push({ id: "agent", label: "Create the agent", state: "waiting" });
    if (pack.training) plan.push({ id: "training", label: "Add the training notes", state: "waiting" });
    setSteps(plan);
    setBusy(true);

    let kbId: string | null = null;
    let agentId: string | null = null;
    let current = plan[0].id;
    const start = (id: string, detail?: string) => {
      current = id;
      update(id, { state: "running", detail });
    };
    try {
      if (pack.files.length > 0) {
        start("kb");
        const kb = await apiSend<{ id: string }>("POST", "knowledge-bases", { name: `${agentIn.name} knowledge`.slice(0, 200) });
        kbId = kb.id;
        update("kb", { state: "done" });

        start("upload");
        for (const [i, f] of pack.files.entries()) {
          update("upload", { detail: f.name, progress: i / pack.files.length });
          const blob = new File([f.bytes.slice()], f.name, { type: f.type });
          await apiUpload(`knowledge-bases/${kbId}/documents`, blob, (p) =>
            update("upload", { progress: (i + p) / pack.files.length }),
          );
        }
        update("upload", { state: "done", detail: undefined, progress: undefined });

        start("index");
        const deadline = Date.now() + INDEX_TIMEOUT_MS;
        for (;;) {
          const { documents } = await apiGet<{ documents: KbDocument[] }>(`knowledge-bases/${kbId}/documents`);
          const failed = documents.find((d) => d.status === "failed");
          if (failed) throw new Error(`"${failed.filename}" could not be read: ${failed.error ?? "unknown error"}`);
          const ready = documents.filter((d) => d.status === "ready").length;
          update("index", { detail: `${ready} of ${pack.files.length} ready`, progress: ready / pack.files.length });
          if (ready >= pack.files.length) break;
          if (Date.now() > deadline) throw new Error("Indexing is taking longer than 10 minutes.");
          await new Promise((r) => setTimeout(r, 1500));
        }
        update("index", { state: "done", progress: undefined });
      }

      start("agent");
      const agent = await apiSend<Agent>("POST", "agents", { ...agentIn, knowledge_base_id: kbId });
      agentId = agent.id;
      update("agent", { state: "done" });

      if (pack.training) {
        start("training");
        await apiSend("PUT", `agents/${agent.id}/training`, { training_notes: pack.training });
        update("training", { state: "done" });
      }
      toast.success(`${agent.name} is ready`);
      router.push(`/agents/${agent.id}`);
    } catch (e) {
      update(current, { state: "failed", detail: errorMessage(e), progress: undefined });
      // Leave nothing half-made behind.
      if (agentId) await apiSend("DELETE", `agents/${agentId}`).catch(() => undefined);
      if (kbId) await apiSend("DELETE", `knowledge-bases/${kbId}`).catch(() => undefined);
      toast.error("Import failed; nothing was kept", { description: errorMessage(e) });
      setBusy(false);
    }
  }

  const agent = pack?.agent;
  const canCreate = !!agent && pack.errors.length === 0 && !busy;

  return (
    <div className="grid gap-4 lg:grid-cols-2">
      <Card>
        <CardHeader>
          <CardTitle>1. Ask ChatGPT or Claude to build the pack</CardTitle>
          <CardDescription>
            Copy this prompt into a new chat, then attach or paste what you know about the business: brochure, price list, website text,
            FAQs. The AI asks for anything missing and gives you a ZIP file.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <Textarea readOnly value={PACK_PROMPT} rows={12} className="font-mono text-xs" onFocus={(e) => e.currentTarget.select()} />
        </CardContent>
        <CardFooter className="flex-wrap gap-2">
          <Button onClick={copyPrompt}>
            {copied ? <CheckIcon data-icon="inline-start" /> : <CopyIcon data-icon="inline-start" />}
            {copied ? "Copied" : "Copy prompt"}
          </Button>
          <p className="text-xs text-muted-foreground">If the AI can&apos;t make files, it prints them; save them into a folder and zip it.</p>
        </CardFooter>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>2. Upload the ZIP</CardTitle>
          <CardDescription>It is checked in your browser first. Nothing is saved until you press Create.</CardDescription>
        </CardHeader>
        <CardContent className="grid gap-4">
          <button
            type="button"
            disabled={busy}
            onClick={() => input.current?.click()}
            onDragOver={(e) => {
              e.preventDefault();
              setDragging(true);
            }}
            onDragLeave={() => setDragging(false)}
            onDrop={(e) => {
              e.preventDefault();
              setDragging(false);
              void load(e.dataTransfer.files[0]);
            }}
            className={cn(
              "flex flex-col items-center gap-2 rounded-lg border-2 border-dashed p-6 text-center text-sm transition-colors",
              dragging ? "border-primary bg-primary/5" : "hover:bg-muted/50",
            )}
          >
            <FileArchiveIcon className="size-8 text-muted-foreground" />
            <span className="font-medium">{fileName ?? "Drop the pack ZIP here or click to choose"}</span>
            <span className="text-xs text-muted-foreground">agent.json, a knowledge folder (.md, .txt, .pdf) and an optional train.md</span>
          </button>
          <input
            ref={input}
            type="file"
            accept=".zip,application/zip"
            className="hidden"
            onChange={(e) => {
              void load(e.target.files?.[0]);
              e.target.value = "";
            }}
          />

          {pack && pack.errors.length > 0 && (
            <Alert variant="destructive">
              <XCircleIcon />
              <AlertTitle>Fix these before importing</AlertTitle>
              <AlertDescription>
                <ul className="list-disc space-y-1 pl-4">
                  {pack.errors.map((e) => (
                    <li key={e} className="[overflow-wrap:anywhere]">
                      {e}
                    </li>
                  ))}
                </ul>
              </AlertDescription>
            </Alert>
          )}
          {pack && pack.warnings.length > 0 && (
            <Alert>
              <AlertTriangleIcon />
              <AlertTitle>Worth checking</AlertTitle>
              <AlertDescription>
                <ul className="list-disc space-y-1 pl-4">
                  {pack.warnings.map((w) => (
                    <li key={w} className="[overflow-wrap:anywhere]">
                      {w}
                    </li>
                  ))}
                </ul>
              </AlertDescription>
            </Alert>
          )}
          {pack && pack.errors.length === 0 && agent && (
            <Alert>
              <ShieldCheckIcon />
              <AlertTitle>Passed the safety check</AlertTitle>
              <AlertDescription>
                Nothing in this pack tries to change the agent&apos;s rules. The AI and recording notice is still played first on every
                call.
              </AlertDescription>
            </Alert>
          )}
        </CardContent>
      </Card>

      {agent && (
        <Card className="lg:col-span-2">
          <CardHeader>
            <CardTitle>3. Check and create</CardTitle>
            <CardDescription>You can change anything on the agent page afterwards.</CardDescription>
          </CardHeader>
          <CardContent className="grid gap-4 md:grid-cols-2">
            <PackPreview agent={agent} pack={pack} />
            {steps && (
              <ol className="grid content-start gap-3 md:col-span-2">
                {steps.map((s) => (
                  <li key={s.id} className="grid gap-1.5">
                    <div className="flex items-center gap-2 text-sm">
                      <StepIcon state={s.state} />
                      <span className={cn(s.state === "waiting" && "text-muted-foreground")}>{s.label}</span>
                      {s.detail && <span className="min-w-0 truncate text-xs text-muted-foreground">· {s.detail}</span>}
                    </div>
                    {s.state === "running" && s.progress !== undefined && <Progress value={s.progress * 100} className="h-1.5" />}
                  </li>
                ))}
              </ol>
            )}
          </CardContent>
          <CardFooter>
            <Button disabled={!canCreate} onClick={create}>
              {busy ? <Loader2Icon data-icon="inline-start" className="animate-spin" /> : <PackageIcon data-icon="inline-start" />}
              {busy ? "Creating…" : "Create agent"}
            </Button>
          </CardFooter>
        </Card>
      )}
    </div>
  );
}

function StepIcon({ state }: { state: StepState }) {
  if (state === "done") return <CheckCircle2Icon className="size-4 text-emerald-600" />;
  if (state === "failed") return <XCircleIcon className="size-4 text-destructive" />;
  if (state === "running") return <Loader2Icon className="size-4 animate-spin" />;
  return <CircleIcon className="size-4 text-muted-foreground" />;
}

function PackPreview({ agent, pack }: { agent: PackAgent; pack: ParsedPack }) {
  return (
    <>
      <dl className="grid content-start gap-3 text-sm">
        <div>
          <dt className="text-xs text-muted-foreground">Name</dt>
          <dd className="font-medium">{agent.name || "—"}</dd>
        </div>
        {agent.description && (
          <div>
            <dt className="text-xs text-muted-foreground">Description</dt>
            <dd>{agent.description}</dd>
          </div>
        )}
        <div>
          <dt className="text-xs text-muted-foreground">Greeting (after the AI and recording notice)</dt>
          <dd>{agent.greeting ?? "The default greeting for the language"}</dd>
        </div>
        <div className="flex flex-wrap gap-1.5">
          <Badge variant="secondary">{languageName(agent.language)}</Badge>
          <Badge variant="secondary">
            {agent.fields.length} field{agent.fields.length === 1 ? "" : "s"}
          </Badge>
          {agent.booking && <Badge variant="secondary">Booking</Badge>}
          {agent.max_duration_secs && <Badge variant="secondary">Up to {Math.round(agent.max_duration_secs / 60)} min</Badge>}
          {pack.training && <Badge variant="secondary">train.md</Badge>}
        </div>
        {agent.fields.length > 0 && (
          <div>
            <dt className="text-xs text-muted-foreground">Details it collects</dt>
            <dd className="text-muted-foreground">{agent.fields.map((f) => f.label).join(", ")}</dd>
          </div>
        )}
        <div>
          <dt className="text-xs text-muted-foreground">Knowledge files</dt>
          <dd>
            {pack.files.length === 0 ? (
              <span className="text-muted-foreground">None</span>
            ) : (
              <ul className="grid gap-1">
                {pack.files.map((f) => (
                  <li key={f.name} className="flex min-w-0 items-center gap-2">
                    <FileTextIcon className="size-3.5 shrink-0 text-muted-foreground" />
                    <span className="min-w-0 truncate">{f.name}</span>
                    <span className="shrink-0 text-xs text-muted-foreground tabular-nums">{formatBytes(f.bytes.byteLength)}</span>
                  </li>
                ))}
              </ul>
            )}
          </dd>
        </div>
      </dl>
      <div className="grid min-w-0 content-start gap-1">
        <span className="text-xs text-muted-foreground">Prompt</span>
        <pre className="max-h-80 overflow-y-auto rounded-lg bg-muted p-3 font-sans text-xs leading-relaxed [overflow-wrap:anywhere] whitespace-pre-wrap">
          {agent.prompt}
        </pre>
      </div>
    </>
  );
}

/** Downloads an agent as a pack ZIP: agent.json, its knowledge files and train.md. */
export function ExportPackButton({ agent }: { agent: Agent }) {
  const [busy, setBusy] = useState(false);

  async function exportPack() {
    setBusy(true);
    try {
      const files: { name: string; bytes: Uint8Array }[] = [];
      const kb = agent.knowledge_base_id;
      if (kb) {
        const { documents } = await apiGet<{ documents: KbDocument[] }>(`knowledge-bases/${kb}/documents`);
        const used = new Set<string>();
        for (const d of documents.filter((x) => x.status === "ready")) {
          const detail = await apiGet<KbDocumentDetail>(`knowledge-bases/${kb}/documents/${d.id}`);
          let data: Uint8Array;
          if (detail.text !== null) data = strToU8(detail.text);
          else {
            const res = await fetch(`/api/kb-files/${kb}/${d.id}`, { cache: "no-store" });
            if (!res.ok) throw new ApiError(res.status, `Could not read "${d.filename}"`);
            data = new Uint8Array(await res.arrayBuffer());
          }
          let name = d.filename;
          for (let n = 2; used.has(name.toLowerCase()); n++) name = d.filename.replace(/(\.[^.]+)?$/, `-${n}$1`);
          used.add(name.toLowerCase());
          files.push({ name, bytes: data });
        }
      }
      const training = await apiGet<Training>(`agents/${agent.id}/training`);
      const zip = buildPack(
        {
          name: agent.name,
          description: agent.description,
          prompt: agent.prompt,
          greeting: agent.greeting,
          language: agent.language,
          booking: agent.booking,
          max_duration_secs: agent.max_duration_secs,
          fields: agent.fields.map((f) => ({ ...f, options: f.type === "choice" ? f.options : undefined })),
        },
        files,
        training.training_notes,
      );
      const url = URL.createObjectURL(new Blob([zip.slice()], { type: "application/zip" }));
      const a = document.createElement("a");
      a.href = url;
      a.download = packFileName(agent.name);
      a.click();
      setTimeout(() => URL.revokeObjectURL(url), 10_000);
      toast.success("Pack downloaded", { description: `${files.length} knowledge file${files.length === 1 ? "" : "s"}` });
    } catch (e) {
      toast.error("Could not export the pack", { description: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <Button variant="outline" disabled={busy} onClick={exportPack}>
      {busy ? <Loader2Icon data-icon="inline-start" className="animate-spin" /> : <DownloadIcon data-icon="inline-start" />}
      Export pack
    </Button>
  );
}
