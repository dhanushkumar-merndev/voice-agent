"use client";

import { useRef, useState } from "react";
import Link from "next/link";
import {
  CheckIcon,
  DownloadIcon,
  GraduationCapIcon,
  RefreshCwIcon,
  SparklesIcon,
  ThumbsDownIcon,
  ThumbsUpIcon,
  UploadIcon,
  XIcon,
} from "lucide-react";
import { toast } from "sonner";

import { ErrorAlert, LoadingBlock } from "@/components/common";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { useApi } from "@/hooks/use-api";
import { apiSend, errorMessage } from "@/lib/api";
import { humanize, relative } from "@/lib/format";
import { cn } from "@/lib/utils";
import type { CallDetail, CallReview as Review, Lesson, Rating, Training } from "@/lib/types";

const SCORE_TONE = (s: number) =>
  s >= 8
    ? "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300"
    : s >= 5
      ? "bg-sky-500/15 text-sky-700 dark:text-sky-300"
      : "bg-amber-500/15 text-amber-700 dark:text-amber-300";

/** A review score out of 10, coloured by how good it is. */
export function ScoreBadge({ score, label }: { score: number | null | undefined; label?: string }) {
  if (score === null || score === undefined) return null;
  return (
    <Badge variant="secondary" className={cn("font-normal tabular-nums", SCORE_TONE(score))}>
      {label ? `${label} ` : ""}
      {Number.isInteger(score) ? score : score.toFixed(1)}/10
    </Badge>
  );
}

/** One lesson. A suggested one can be reworded, then applied or rejected. */
function LessonRow({
  lesson,
  agentId,
  onChange,
  showCall,
}: {
  lesson: Lesson;
  agentId: string;
  onChange: () => void;
  showCall?: boolean;
}) {
  const [text, setText] = useState(lesson.lesson);
  const [busy, setBusy] = useState<"apply" | "reject" | null>(null);
  const suggested = lesson.status === "suggested";
  const tooShort = text.trim().length < 3;
  const tooLong = text.length > 300;

  async function decide(action: "apply" | "reject") {
    setBusy(action);
    try {
      if (action === "apply") {
        const reworded = text.trim() !== lesson.lesson ? { lesson: text.trim() } : undefined;
        await apiSend("POST", `agents/${agentId}/lessons/${lesson.id}/apply`, reworded);
        toast.success("Lesson added to train.md", { description: "The agent follows it from the next call." });
      } else {
        await apiSend("POST", `agents/${agentId}/lessons/${lesson.id}/reject`);
        toast.success("Lesson rejected");
      }
      onChange();
    } catch (e) {
      toast.error(action === "apply" ? "Could not apply the lesson" : "Could not reject the lesson", { description: errorMessage(e) });
    } finally {
      setBusy(null);
    }
  }

  return (
    <li className="grid gap-2 rounded-lg border p-3">
      {lesson.mistake && (
        <p className="text-xs text-muted-foreground">
          <span className="font-medium text-foreground">What went wrong:</span> {lesson.mistake}
        </p>
      )}
      {suggested ? (
        <>
          <Label htmlFor={`lesson-${lesson.id}`} className="sr-only">
            Lesson
          </Label>
          <Textarea
            id={`lesson-${lesson.id}`}
            value={text}
            onChange={(e) => setText(e.target.value)}
            rows={2}
            aria-invalid={tooShort || tooLong}
            className="min-h-0 text-sm"
          />
        </>
      ) : (
        <p className={cn("text-sm", lesson.status === "rejected" && "text-muted-foreground line-through")}>{lesson.lesson}</p>
      )}
      <div className="flex flex-wrap items-center gap-2 text-xs text-muted-foreground">
        <Badge variant="outline" className="font-normal">
          {lesson.source === "feedback" ? "From your feedback" : "From call review"}
        </Badge>
        <span>{relative(lesson.created_at)}</span>
        {showCall && lesson.call_id && (
          <Link href={`/calls/${lesson.call_id}`} className="underline">
            View call
          </Link>
        )}
        {tooLong && <span className="text-destructive">{text.length}/300 characters</span>}
        {suggested && (
          <div className="ml-auto flex gap-2">
            <Button size="sm" variant="ghost" disabled={busy !== null} onClick={() => decide("reject")}>
              <XIcon data-icon="inline-start" />
              Reject
            </Button>
            <Button size="sm" disabled={busy !== null || tooShort || tooLong} onClick={() => decide("apply")}>
              <CheckIcon data-icon="inline-start" />
              {busy === "apply" ? "Applying…" : "Apply"}
            </Button>
          </div>
        )}
        {!suggested && (
          <Badge variant="secondary" className="ml-auto font-normal capitalize">
            {lesson.status}
          </Badge>
        )}
      </div>
    </li>
  );
}

/** Agent page: lessons to approve, the agent's train.md and the shared lessons. */
export function AgentTraining({ agentId }: { agentId: string }) {
  const training = useApi<Training>(`agents/${agentId}/training`);
  const lessons = useApi<{ lessons: Lesson[] }>(`agents/${agentId}/lessons`);
  const reloadAll = () => {
    training.reload();
    lessons.reload();
  };
  const suggested = lessons.data?.lessons.filter((l) => l.status === "suggested") ?? [];
  const decided = lessons.data?.lessons.filter((l) => l.status !== "suggested").slice(0, 10) ?? [];

  return (
    <section id="training" className="mt-6 grid scroll-mt-20 gap-4">
      <div>
        <h2 className="flex items-center gap-2 text-lg font-semibold">
          <GraduationCapIcon className="size-5" />
          Training
        </h2>
        <p className="text-sm text-muted-foreground">
          After every call a reviewer scores it and suggests lessons. Approved lessons go into this agent&apos;s train.md, which is added to its
          instructions on every call.
        </p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Lessons to review</CardTitle>
          <CardDescription>
            {suggested.length === 0 ? "Nothing waiting. New suggestions appear here after calls." : `${suggested.length} waiting. Edit the wording before applying if you like.`}
          </CardDescription>
        </CardHeader>
        <CardContent>
          {lessons.error && !lessons.data && <ErrorAlert error={lessons.error} title="Could not load lessons" />}
          {lessons.loading && <LoadingBlock rows={2} />}
          {suggested.length > 0 && (
            <ul className="grid gap-3">
              {suggested.map((l) => (
                <LessonRow key={l.id} lesson={l} agentId={agentId} onChange={reloadAll} showCall />
              ))}
            </ul>
          )}
        </CardContent>
      </Card>

      {training.error && !training.data && <ErrorAlert error={training.error} title="Could not load training notes" />}
      {training.loading && <LoadingBlock rows={3} />}
      {training.data && <TrainMd agentId={agentId} key={training.data.training_notes} training={training.data} onSaved={reloadAll} />}

      {decided.length > 0 && (
        <Card>
          <CardHeader>
            <CardTitle>Recent decisions</CardTitle>
            <CardDescription>The last {decided.length} lessons applied or rejected</CardDescription>
          </CardHeader>
          <CardContent>
            <ul className="grid gap-3">
              {decided.map((l) => (
                <LessonRow key={l.id} lesson={l} agentId={agentId} onChange={reloadAll} showCall />
              ))}
            </ul>
          </CardContent>
        </Card>
      )}

      {training.data?.shared_notes.trim() && (
        <Card>
          <CardHeader>
            <CardTitle>Lessons for every agent</CardTitle>
            <CardDescription>From prompts/train.md on the server; edit that file and restart to change them.</CardDescription>
          </CardHeader>
          <CardContent>
            <pre className="max-h-64 overflow-auto rounded-lg bg-muted p-3 text-xs whitespace-pre-wrap">{training.data.shared_notes.trim()}</pre>
          </CardContent>
        </Card>
      )}
    </section>
  );
}

/** The agent's train.md: edit, upload, download, and the auto-apply switch. */
function TrainMd({ agentId, training, onSaved }: { agentId: string; training: Training; onSaved: () => void }) {
  const [notes, setNotes] = useState(training.training_notes);
  const [auto, setAuto] = useState(training.auto_apply_lessons);
  const [saving, setSaving] = useState(false);
  const fileRef = useRef<HTMLInputElement>(null);
  const over = notes.length > training.max_chars;
  const dirty = notes !== training.training_notes;

  async function save(body: { training_notes?: string; auto_apply_lessons?: boolean }) {
    setSaving(true);
    try {
      await apiSend("PUT", `agents/${agentId}/training`, body);
      toast.success(body.training_notes !== undefined ? "train.md saved" : body.auto_apply_lessons ? "Lessons now apply automatically" : "Lessons now wait for approval");
      onSaved();
    } catch (e) {
      toast.error("Could not save", { description: errorMessage(e) });
      if (body.auto_apply_lessons !== undefined) setAuto(!body.auto_apply_lessons);
    } finally {
      setSaving(false);
    }
  }

  async function upload(file: File | undefined) {
    if (!file) return;
    if (file.size > 64 * 1024) {
      toast.error("That file is too large", { description: `train.md holds at most ${training.max_chars.toLocaleString()} characters.` });
      return;
    }
    setNotes(await file.text());
    toast.info(`Loaded ${file.name}`, { description: "Check it, then press Save." });
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>train.md</CardTitle>
        <CardDescription>Plain lines the agent follows on every call. Keep each one short and say what to do.</CardDescription>
        <CardAction className="flex gap-1">
          <Button variant="ghost" size="icon-sm" aria-label="Upload train.md" title="Upload a .md file" onClick={() => fileRef.current?.click()}>
            <UploadIcon />
          </Button>
          <Button variant="ghost" size="icon-sm" aria-label="Download train.md" title="Download train.md" asChild>
            <a href={`/api/v1/agents/${agentId}/train.md`} download="train.md">
              <DownloadIcon />
            </a>
          </Button>
          <input
            ref={fileRef}
            type="file"
            accept=".md,.txt,text/markdown,text/plain"
            className="hidden"
            onChange={(e) => {
              void upload(e.target.files?.[0]);
              e.target.value = "";
            }}
          />
        </CardAction>
      </CardHeader>
      <CardContent className="grid gap-3">
        <Textarea
          aria-label="train.md"
          value={notes}
          onChange={(e) => setNotes(e.target.value)}
          rows={10}
          placeholder={"## Lessons from calls\n\n- Ask for the budget before ending the call."}
          aria-invalid={over}
          className="font-mono text-xs"
        />
        <div className="flex flex-wrap items-center gap-3">
          <span className={cn("text-xs tabular-nums", over ? "text-destructive" : "text-muted-foreground")}>
            {notes.length.toLocaleString()} / {training.max_chars.toLocaleString()} characters
          </span>
          <div className="ml-auto flex gap-2">
            {dirty && (
              <Button variant="ghost" size="sm" disabled={saving} onClick={() => setNotes(training.training_notes)}>
                Undo changes
              </Button>
            )}
            <Button size="sm" disabled={!dirty || over || saving} onClick={() => save({ training_notes: notes })}>
              {saving ? "Saving…" : "Save"}
            </Button>
          </div>
        </div>
        <div className="flex items-start gap-3 rounded-lg border p-3">
          <Switch
            id={`auto-${agentId}`}
            checked={auto}
            disabled={saving}
            onCheckedChange={(v) => {
              setAuto(v);
              void save({ auto_apply_lessons: v });
            }}
          />
          <div className="grid gap-0.5">
            <Label htmlFor={`auto-${agentId}`}>Apply suggested lessons automatically</Label>
            <p className="text-xs text-muted-foreground">
              Off: each suggestion waits for you above. On: suggestions go straight into train.md, so a wrong one can slip in. Check it from time to
              time.
            </p>
          </div>
        </div>
      </CardContent>
    </Card>
  );
}

/** Call page: the reviewer's verdict, your rating and the lessons from this call. */
export function CallReviewCard({ c, reload }: { c: CallDetail; reload: () => void }) {
  const [rating, setRating] = useState<Rating | null>(c.rating);
  const [note, setNote] = useState(c.rating_note ?? "");
  const [saving, setSaving] = useState(false);
  const [reviewing, setReviewing] = useState(false);
  const live = c.in_progress || ["queued", "ringing", "in_progress"].includes(c.status);
  const r: Review | null = c.review;
  const ratingDirty = rating !== c.rating || note.trim() !== (c.rating_note ?? "");

  async function sendFeedback() {
    if (rating === null) return;
    setSaving(true);
    try {
      const res = await apiSend<{ reviewing: boolean }>("POST", `calls/${c.call_id}/feedback`, { rating, note: note.trim() });
      toast.success("Thanks, feedback saved", {
        description: res.reviewing ? "The call is being reviewed again with your note; new lessons appear here in a few seconds." : undefined,
      });
      reload();
      if (res.reviewing) setTimeout(reload, 8000);
    } catch (e) {
      toast.error("Could not save feedback", { description: errorMessage(e) });
    } finally {
      setSaving(false);
    }
  }

  async function reviewAgain() {
    setReviewing(true);
    try {
      await apiSend("POST", `calls/${c.call_id}/review`);
      toast.success("Call reviewed again");
      reload();
    } catch (e) {
      toast.error("Review failed", { description: errorMessage(e) });
    } finally {
      setReviewing(false);
    }
  }

  if (live) return null;
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          Call review
          <ScoreBadge score={r?.score} />
        </CardTitle>
        <CardDescription>
          {r ? `Checked by ${r.model ?? "the reviewer"}${r.reviewed_at ? ` ${relative(r.reviewed_at)}` : ""}` : c.answered_at ? "Arrives a few seconds after the call ends…" : "Not reviewed (call not answered)."}
        </CardDescription>
        {c.answered_at && (
          <CardAction>
            <Button variant="ghost" size="icon-sm" aria-label="Review again" title="Review again" disabled={reviewing} onClick={reviewAgain}>
              <RefreshCwIcon className={cn(reviewing && "animate-spin")} />
            </Button>
          </CardAction>
        )}
      </CardHeader>
      <CardContent className="grid gap-4 text-sm">
        {r && r.went_well.length > 0 && (
          <div>
            <p className="mb-1 text-xs font-medium text-muted-foreground uppercase">Went well</p>
            <ul className="grid list-disc gap-1 pl-4">
              {r.went_well.map((w) => (
                <li key={w}>{w}</li>
              ))}
            </ul>
          </div>
        )}
        {r && r.mistakes.length > 0 && (
          <div>
            <p className="mb-1 text-xs font-medium text-muted-foreground uppercase">Mistakes</p>
            <ul className="grid gap-2">
              {r.mistakes.map((m, i) => (
                <li key={i} className="grid gap-0.5">
                  <span className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
                    <Badge variant="outline" className="font-normal">
                      {humanize(m.kind)}
                    </Badge>
                    {m.turn !== null && <span>turn {m.turn}</span>}
                  </span>
                  <span>{m.what}</span>
                </li>
              ))}
            </ul>
          </div>
        )}
        {c.lessons.length > 0 && c.agent_id && (
          <div>
            <p className="mb-1 flex items-center gap-1 text-xs font-medium text-muted-foreground uppercase">
              <SparklesIcon className="size-3" />
              Suggested lessons
            </p>
            <ul className="grid gap-2">
              {c.lessons.map((l) => (
                <LessonRow key={`${l.id}-${l.status}`} lesson={l} agentId={c.agent_id as string} onChange={reload} />
              ))}
            </ul>
          </div>
        )}

        {c.answered_at && (
          <div className="grid gap-2 border-t pt-3">
            <p className="text-xs font-medium text-muted-foreground uppercase">Your rating</p>
            <div className="flex gap-2">
              <Button
                size="sm"
                variant={rating === 1 ? "default" : "outline"}
                aria-pressed={rating === 1}
                onClick={() => setRating(1)}
              >
                <ThumbsUpIcon data-icon="inline-start" />
                Good call
              </Button>
              <Button
                size="sm"
                variant={rating === -1 ? "destructive" : "outline"}
                aria-pressed={rating === -1}
                onClick={() => setRating(-1)}
              >
                <ThumbsDownIcon data-icon="inline-start" />
                Needs work
              </Button>
            </div>
            <Textarea
              aria-label="What went wrong or right"
              value={note}
              onChange={(e) => setNote(e.target.value.slice(0, 500))}
              rows={2}
              placeholder="Optional: what should it do differently? e.g. It should offer the 6-acre farmland when someone asks for 5 acres."
              className="text-sm"
            />
            <div className="flex items-center gap-2">
              <span className="text-xs text-muted-foreground">A thumbs down or a note turns into suggested lessons.</span>
              <Button size="sm" className="ml-auto" disabled={rating === null || !ratingDirty || saving} onClick={sendFeedback}>
                {saving ? "Saving…" : "Save rating"}
              </Button>
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
