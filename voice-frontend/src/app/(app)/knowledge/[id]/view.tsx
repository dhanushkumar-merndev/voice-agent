"use client";

import { useRef, useState } from "react";
import { useRouter } from "next/navigation";
import { EyeIcon, FileTextIcon, Loader2Icon, SearchIcon, Trash2Icon, UploadCloudIcon } from "lucide-react";
import { toast } from "sonner";

import { ErrorAlert, LoadingBlock, PageHeader } from "@/components/common";
import { DocumentSheet } from "@/components/kb-document";
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
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Progress } from "@/components/ui/progress";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { useApi } from "@/hooks/use-api";
import { apiSend, apiUpload, errorMessage } from "@/lib/api";
import { bytes, relative } from "@/lib/format";
import { cn } from "@/lib/utils";
import type { KbDocument, KnowledgeBase, SearchHit } from "@/lib/types";

const ACCEPT = [".pdf", ".md", ".markdown", ".txt"];
const MAX_BYTES = 20 * 1024 * 1024;

interface Upload {
  id: string;
  name: string;
  progress: number;
  error?: string;
}

export function KnowledgeBaseView({ id }: { id: string }) {
  const router = useRouter();
  const list = useApi<{ knowledge_bases: KnowledgeBase[]; embedding_model_ready: boolean }>("knowledge-bases");
  const docs = useApi<{ documents: KbDocument[] }>(`knowledge-bases/${id}/documents`, {
    // Ingestion runs in the background: poll until every document settles.
    refreshMs: (d) => (d?.documents.some((x) => x.status === "pending" || x.status === "processing") ? 1500 : undefined),
  });
  const kb = list.data?.knowledge_bases.find((k) => k.id === id);
  const [uploads, setUploads] = useState<Upload[]>([]);
  const [dragging, setDragging] = useState(false);
  const [viewing, setViewing] = useState<KbDocument | null>(null);
  const input = useRef<HTMLInputElement>(null);

  async function upload(files: FileList | File[]) {
    const accepted: File[] = [];
    for (const f of Array.from(files)) {
      const ext = f.name.slice(f.name.lastIndexOf(".")).toLowerCase();
      if (!ACCEPT.includes(ext)) toast.error(`${f.name}: only PDF, Markdown and text files`);
      else if (f.size > MAX_BYTES) toast.error(`${f.name}: larger than 20 MB`);
      else accepted.push(f);
    }
    // One at a time keeps the embedding queue and the browser connection steady.
    for (const f of accepted) {
      const uid = crypto.randomUUID();
      setUploads((u) => [...u, { id: uid, name: f.name, progress: 0 }]);
      try {
        await apiUpload(`knowledge-bases/${id}/documents`, f, (p) =>
          setUploads((u) => u.map((x) => (x.id === uid ? { ...x, progress: p } : x))),
        );
        setUploads((u) => u.filter((x) => x.id !== uid));
        docs.reload();
      } catch (e) {
        setUploads((u) => u.map((x) => (x.id === uid ? { ...x, error: errorMessage(e) } : x)));
      }
    }
    list.reload();
  }

  async function removeDoc(doc: KbDocument) {
    try {
      await apiSend("DELETE", `knowledge-bases/${id}/documents/${doc.id}`);
      toast.success(`Removed ${doc.filename}`);
      docs.reload();
      list.reload();
    } catch (e) {
      toast.error("Not removed", { description: errorMessage(e) });
    }
  }

  async function removeKb() {
    try {
      await apiSend("DELETE", `knowledge-bases/${id}`);
      toast.success("Knowledge base deleted");
      router.push("/knowledge");
    } catch (e) {
      toast.error("Not deleted", { description: errorMessage(e) });
    }
  }

  if (list.loading || docs.loading) return <LoadingBlock rows={4} />;
  if (!kb) return <ErrorAlert error={list.error ?? docs.error ?? "Knowledge base not found"} title="Could not load the knowledge base" />;
  const ready = list.data?.embedding_model_ready ?? false;
  const names = Object.fromEntries((docs.data?.documents ?? []).map((d) => [d.id, d.filename]));

  return (
    <>
      <PageHeader
        title={kb.name}
        description={`${kb.ready_documents} ready document${kb.ready_documents === 1 ? "" : "s"} · ${kb.chunks} chunks · ${bytes(kb.size_bytes)}`}
        actions={
          <AlertDialog>
            <AlertDialogTrigger asChild>
              <Button variant="ghost" className="text-destructive">
                <Trash2Icon data-icon="inline-start" />
                Delete
              </Button>
            </AlertDialogTrigger>
            <AlertDialogContent>
              <AlertDialogHeader>
                <AlertDialogTitle>Delete {kb.name}?</AlertDialogTitle>
                <AlertDialogDescription>
                  All its documents and embeddings are removed. Agents using it will answer without it.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogCancel>Cancel</AlertDialogCancel>
                <AlertDialogAction onClick={removeKb}>Delete</AlertDialogAction>
              </AlertDialogFooter>
            </AlertDialogContent>
          </AlertDialog>
        }
      />

      <div className="grid gap-4 lg:grid-cols-[1fr_24rem]">
        <div className="grid content-start gap-4">
          <button
            type="button"
            disabled={!ready}
            onClick={() => input.current?.click()}
            onDragOver={(e) => {
              e.preventDefault();
              setDragging(true);
            }}
            onDragLeave={() => setDragging(false)}
            onDrop={(e) => {
              e.preventDefault();
              setDragging(false);
              if (ready) void upload(e.dataTransfer.files);
            }}
            className={cn(
              "flex flex-col items-center justify-center gap-2 rounded-xl border-2 border-dashed p-8 text-center transition-colors disabled:cursor-not-allowed disabled:opacity-60",
              dragging ? "border-primary bg-primary/5" : "hover:border-foreground/30 hover:bg-muted/40",
            )}
          >
            <UploadCloudIcon className="size-8 text-muted-foreground" />
            <span className="font-medium">{ready ? "Drop files here or click to choose" : "Uploads disabled: embedding model missing"}</span>
            <span className="text-sm text-muted-foreground">PDF, Markdown or text · up to 20 MB each · several at once</span>
          </button>
          <input
            ref={input}
            type="file"
            multiple
            accept={ACCEPT.join(",")}
            className="hidden"
            onChange={(e) => {
              if (e.target.files) void upload(e.target.files);
              e.target.value = "";
            }}
          />

          {uploads.length > 0 && (
            <Card>
              <CardContent className="grid gap-3">
                {uploads.map((u) => (
                  <div key={u.id} className="grid gap-1">
                    <div className="flex justify-between gap-2 text-sm">
                      <span className="truncate">{u.name}</span>
                      <span className={cn("tabular-nums", u.error ? "text-destructive" : "text-muted-foreground")}>
                        {u.error ?? `${Math.round(u.progress * 100)}%`}
                      </span>
                    </div>
                    {!u.error && <Progress value={u.progress * 100} />}
                  </div>
                ))}
              </CardContent>
            </Card>
          )}

          <Card className="py-0">
            <CardContent className="px-0">
              {docs.data?.documents.length === 0 ? (
                <p className="p-8 text-center text-sm text-muted-foreground">No documents yet.</p>
              ) : (
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead className="pl-4">Document</TableHead>
                      <TableHead>Status</TableHead>
                      <TableHead className="text-right">Chunks</TableHead>
                      <TableHead className="hidden text-right sm:table-cell">Size</TableHead>
                      <TableHead className="hidden md:table-cell">Added</TableHead>
                      <TableHead className="w-20 pr-4" />
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {docs.data?.documents.map((d) => (
                      <TableRow key={d.id}>
                        <TableCell className="max-w-56 pl-4">
                          <button
                            type="button"
                            className="flex max-w-full items-center gap-2 text-left hover:underline"
                            title={`View ${d.filename}`}
                            onClick={() => setViewing(d)}
                          >
                            <FileTextIcon className="size-4 shrink-0 text-muted-foreground" />
                            <span className="truncate">{d.filename}</span>
                          </button>
                          {d.error && <p className="mt-1 text-xs whitespace-normal text-destructive">{d.error}</p>}
                        </TableCell>
                        <TableCell>
                          <DocStatus status={d.status} />
                        </TableCell>
                        <TableCell className="text-right tabular-nums">{d.chunks}</TableCell>
                        <TableCell className="hidden text-right tabular-nums sm:table-cell">{bytes(d.size_bytes)}</TableCell>
                        <TableCell className="hidden text-sm text-muted-foreground md:table-cell">{relative(d.created_at)}</TableCell>
                        <TableCell className="pr-4">
                          <div className="flex justify-end gap-1">
                            <Button variant="ghost" size="icon-sm" aria-label={`View ${d.filename}`} title="View" onClick={() => setViewing(d)}>
                              <EyeIcon />
                            </Button>
                            <Button variant="ghost" size="icon-sm" aria-label={`Remove ${d.filename}`} title="Remove" onClick={() => removeDoc(d)}>
                              <Trash2Icon />
                            </Button>
                          </div>
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              )}
            </CardContent>
          </Card>
        </div>

        <SearchBox kbId={id} names={names} disabled={!ready || kb.chunks === 0} />
      </div>
      <DocumentSheet kbId={id} doc={viewing} onClose={() => setViewing(null)} />
    </>
  );
}

function DocStatus({ status }: { status: KbDocument["status"] }) {
  if (status === "ready") return <Badge variant="outline">Ready</Badge>;
  if (status === "failed") return <Badge variant="destructive">Failed</Badge>;
  return (
    <Badge variant="secondary" className="gap-1">
      <Loader2Icon className="size-3 animate-spin" />
      {status === "pending" ? "Queued" : "Embedding"}
    </Badge>
  );
}

function SearchBox({ kbId, names, disabled }: { kbId: string; names: Record<string, string>; disabled: boolean }) {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<{ results: SearchHit[]; took_ms: number }>();
  const [busy, setBusy] = useState(false);

  async function search(e: React.FormEvent) {
    e.preventDefault();
    if (!query.trim()) return;
    setBusy(true);
    try {
      setHits(await apiSend("POST", `knowledge-bases/${kbId}/search`, { query: query.trim(), top_k: 5 }));
    } catch (err) {
      toast.error("Search failed", { description: errorMessage(err) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card className="content-start lg:sticky lg:top-20 lg:self-start">
      <CardHeader>
        <CardTitle>Test a question</CardTitle>
        <CardDescription>What the agent would find when a caller asks this.</CardDescription>
      </CardHeader>
      <CardContent className="grid gap-3">
        <form onSubmit={search} className="flex gap-2">
          <Input placeholder="What are the timings on Sunday?" value={query} onChange={(e) => setQuery(e.target.value)} disabled={disabled} />
          <Button type="submit" size="icon" aria-label="Search" disabled={disabled || busy || !query.trim()}>
            {busy ? <Loader2Icon className="animate-spin" /> : <SearchIcon />}
          </Button>
        </form>
        {disabled && <p className="text-xs text-muted-foreground">Upload a document first.</p>}
        {hits && (
          <div className="grid gap-2">
            <p className="text-xs text-muted-foreground">
              {hits.results.length} match{hits.results.length === 1 ? "" : "es"} in {hits.took_ms} ms
            </p>
            {hits.results.map((h, i) => (
              <div key={i} className="rounded-lg border p-3 text-sm">
                <div className="mb-1 flex items-center justify-between gap-2 text-xs text-muted-foreground">
                  <span className="truncate">{(h.document_id && names[h.document_id]) || "document"}</span>
                  <span className="tabular-nums">{Math.round(h.score * 100)}% match</span>
                </div>
                <p className="line-clamp-6 whitespace-pre-wrap">{h.content}</p>
              </div>
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  );
}
