"use client";

import { useState } from "react";
import { CheckIcon, Loader2Icon, RotateCcwIcon, SparklesIcon, XIcon } from "lucide-react";
import { toast } from "sonner";

import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/textarea";
import { findTampering } from "@/lib/agent-pack";
import { apiSend, errorMessage } from "@/lib/api";
import { cn } from "@/lib/utils";

const MAX_INSTRUCTION = 1000;
/** The dashboard proxy waits 60 s for the backend. */
const EDIT_TIMEOUT_MS = 65_000;

const EXAMPLES = [
  "Ask for the budget before suggesting anything",
  "If a caller is only free on certain days, don't push other days",
  "Make the rules shorter and simpler",
  "Add: if asked about EMI, say the team will confirm",
];

interface EditResult {
  prompt: string;
  summary: string;
  changed: boolean;
  model: string;
  cost_inr: number;
}

type DiffLine = { kind: "same" | "add" | "del"; text: string };

/** Line diff (longest common subsequence); prompts are a few hundred lines at most. */
function diffLines(before: string, after: string): DiffLine[] {
  const a = before.split("\n");
  const b = after.split("\n");
  const lcs = Array.from({ length: a.length + 1 }, () => new Array<number>(b.length + 1).fill(0));
  for (let i = a.length - 1; i >= 0; i--) {
    for (let j = b.length - 1; j >= 0; j--) {
      lcs[i][j] = a[i] === b[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
    }
  }
  const out: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) {
      out.push({ kind: "same", text: a[i] });
      i++;
      j++;
    } else if (lcs[i + 1][j] >= lcs[i][j + 1]) out.push({ kind: "del", text: a[i++] });
    else out.push({ kind: "add", text: b[j++] });
  }
  while (i < a.length) out.push({ kind: "del", text: a[i++] });
  while (j < b.length) out.push({ kind: "add", text: b[j++] });
  return out;
}

/**
 * "Edit with AI" under the prompt: describe a change, see the AI's version
 * as a diff, and copy it into the form. Nothing is saved until the person
 * presses Save changes.
 */
export function PromptAssistant({ prompt, onApply }: { prompt: string; onApply: (prompt: string) => void }) {
  const [open, setOpen] = useState(false);
  const [instruction, setInstruction] = useState("");
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<(EditResult & { before: string }) | null>(null);
  const [undo, setUndo] = useState<string | null>(null);

  async function suggest() {
    const text = instruction.trim();
    if (!text || !prompt.trim() || busy) return;
    setBusy(true);
    setResult(null);
    try {
      const r = await apiSend<EditResult>("POST", "agents/prompt-edit", { prompt, instruction: text }, EDIT_TIMEOUT_MS);
      setResult({ ...r, before: prompt });
    } catch (e) {
      toast.error("The AI could not edit the prompt", { description: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  }

  const tamper = result ? findTampering(result.prompt, "The suggestion") : null;
  const stale = result !== null && result.before !== prompt;
  const diff = result?.changed ? diffLines(result.before, result.prompt) : [];
  const added = diff.filter((l) => l.kind === "add").length;
  const removed = diff.filter((l) => l.kind === "del").length;

  function apply() {
    if (!result || tamper) return;
    setUndo(result.before);
    onApply(result.prompt);
    setResult(null);
    setInstruction("");
    toast.success("Prompt updated in the form", { description: "Press Save changes to keep it." });
  }

  if (!open) {
    return (
      <div className="flex flex-wrap items-center gap-2">
        <Button type="button" variant="outline" size="sm" onClick={() => setOpen(true)} disabled={!prompt.trim()}>
          <SparklesIcon data-icon="inline-start" />
          Edit with AI
        </Button>
        {undo !== null && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            onClick={() => {
              onApply(undo);
              setUndo(null);
            }}
          >
            <RotateCcwIcon data-icon="inline-start" />
            Undo AI edit
          </Button>
        )}
      </div>
    );
  }

  return (
    <div className="grid gap-3 rounded-lg border bg-muted/30 p-3">
      <div className="flex items-center justify-between gap-2">
        <span className="flex items-center gap-1.5 text-sm font-medium">
          <SparklesIcon className="size-4" />
          Edit with AI
        </span>
        <Button type="button" variant="ghost" size="icon" className="size-7" aria-label="Close" onClick={() => setOpen(false)}>
          <XIcon />
        </Button>
      </div>
      <Textarea
        rows={2}
        maxLength={MAX_INSTRUCTION}
        placeholder="What should change? e.g. Ask the budget before suggesting a plot, and stop pushing other days when the caller is only free on Sundays."
        value={instruction}
        onChange={(e) => setInstruction(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            void suggest();
          }
        }}
      />
      <div className="flex flex-wrap gap-1.5">
        {EXAMPLES.map((x) => (
          <button
            key={x}
            type="button"
            onClick={() => setInstruction(x)}
            className="rounded-full border bg-background px-2.5 py-1 text-xs text-muted-foreground hover:text-foreground"
          >
            {x}
          </button>
        ))}
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <Button type="button" size="sm" disabled={busy || !instruction.trim()} onClick={suggest}>
          {busy ? <Loader2Icon data-icon="inline-start" className="animate-spin" /> : <SparklesIcon data-icon="inline-start" />}
          {busy ? "Writing…" : "Suggest change"}
        </Button>
        <span className="text-xs text-muted-foreground">Nothing is saved until you press Save changes.</span>
      </div>

      {result && (
        <div className="grid gap-2">
          <div className="flex flex-wrap items-baseline justify-between gap-2 text-sm">
            <span className="min-w-0">{result.summary || (result.changed ? "Changed the prompt." : "No change.")}</span>
            <span className="shrink-0 text-xs text-muted-foreground tabular-nums">
              {result.changed && (
                <>
                  <span className="text-emerald-600 dark:text-emerald-400">+{added}</span>{" "}
                  <span className="text-destructive">−{removed}</span> lines ·{" "}
                </>
              )}
              {result.model} · ₹{result.cost_inr.toFixed(2)}
            </span>
          </div>
          {tamper && (
            <Alert variant="destructive">
              <AlertDescription>{tamper.replace("Remove that text and import again.", "It can't be used.")}</AlertDescription>
            </Alert>
          )}
          {stale && (
            <Alert>
              <AlertDescription>You edited the prompt after asking. Using this replaces your newer edits.</AlertDescription>
            </Alert>
          )}
          {result.changed ? (
            <pre className="max-h-96 overflow-y-auto rounded-md border bg-background p-2 font-mono text-xs leading-relaxed">
              {diff.map((l, i) => (
                <div
                  key={i}
                  className={cn(
                    "px-1 [overflow-wrap:anywhere] whitespace-pre-wrap",
                    l.kind === "add" && "bg-emerald-500/15 text-emerald-800 dark:text-emerald-300",
                    l.kind === "del" && "bg-destructive/10 text-destructive line-through",
                    l.kind === "same" && "text-muted-foreground",
                  )}
                >
                  {l.kind === "add" ? "+ " : l.kind === "del" ? "− " : "  "}
                  {l.text || " "}
                </div>
              ))}
            </pre>
          ) : (
            <p className="text-sm text-muted-foreground">The AI left the prompt as it was.</p>
          )}
          <div className="flex flex-wrap gap-2">
            <Button type="button" size="sm" disabled={!result.changed || !!tamper} onClick={apply}>
              <CheckIcon data-icon="inline-start" />
              Use this
            </Button>
            <Button type="button" size="sm" variant="outline" disabled={busy} onClick={suggest}>
              <RotateCcwIcon data-icon="inline-start" />
              Try again
            </Button>
            <Button type="button" size="sm" variant="ghost" onClick={() => setResult(null)}>
              Discard
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}
