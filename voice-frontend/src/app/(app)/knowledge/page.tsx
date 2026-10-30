"use client";

import { useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { AlertTriangleIcon, BookOpenIcon, PlusIcon } from "lucide-react";
import { toast } from "sonner";

import { EmptyState, ErrorAlert, LoadingBlock, PageHeader } from "@/components/common";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle, DialogTrigger } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { useApi } from "@/hooks/use-api";
import { apiSend, errorMessage } from "@/lib/api";
import { bytes, relative } from "@/lib/format";
import type { KnowledgeBase } from "@/lib/types";

export default function KnowledgePage() {
  const router = useRouter();
  const kbs = useApi<{ knowledge_bases: KnowledgeBase[]; embedding_model_ready: boolean }>("knowledge-bases");
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);

  async function create(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    try {
      const kb = await apiSend<{ id: string }>("POST", "knowledge-bases", { name: name.trim() });
      toast.success("Knowledge base created");
      router.push(`/knowledge/${kb.id}`);
    } catch (err) {
      toast.error("Not created", { description: errorMessage(err) });
      setBusy(false);
    }
  }

  return (
    <>
      <PageHeader
        title="Knowledge bases"
        description="Upload price lists, FAQs and brochures. They are split into chunks, embedded (bge-m3, 1024-d) and stored in pgvector; agents search them during calls."
        actions={
          <Dialog open={open} onOpenChange={setOpen}>
            <DialogTrigger asChild>
              <Button>
                <PlusIcon data-icon="inline-start" />
                New knowledge base
              </Button>
            </DialogTrigger>
            <DialogContent>
              <form onSubmit={create} className="grid gap-4">
                <DialogHeader>
                  <DialogTitle>New knowledge base</DialogTitle>
                  <DialogDescription>Group the documents one agent answers from, e.g. one per business or product.</DialogDescription>
                </DialogHeader>
                <div className="grid gap-2">
                  <Label htmlFor="kb-name">Name</Label>
                  <Input id="kb-name" autoFocus required maxLength={200} placeholder="Star Homes projects" value={name} onChange={(e) => setName(e.target.value)} />
                </div>
                <DialogFooter>
                  <Button type="submit" disabled={busy || !name.trim()}>
                    {busy ? "Creating…" : "Create"}
                  </Button>
                </DialogFooter>
              </form>
            </DialogContent>
          </Dialog>
        }
      />
      {kbs.data && !kbs.data.embedding_model_ready && (
        <Alert className="mb-4">
          <AlertTriangleIcon />
          <AlertTitle>Embedding model missing</AlertTitle>
          <AlertDescription>Uploads are disabled until the backend has the bge-m3 model (run scripts/download_models.sh).</AlertDescription>
        </Alert>
      )}
      {kbs.error && !kbs.data && <ErrorAlert error={kbs.error} title="Could not load knowledge bases" />}
      {kbs.loading && <LoadingBlock rows={3} />}
      {kbs.data?.knowledge_bases.length === 0 && (
        <EmptyState icon={BookOpenIcon} title="No knowledge bases yet">
          Create one, then drop in .pdf, .md or .txt files.
        </EmptyState>
      )}
      <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
        {kbs.data?.knowledge_bases.map((k) => (
          <Link key={k.id} href={`/knowledge/${k.id}`} className="group">
            <Card className="h-full transition-colors group-hover:border-foreground/20">
              <CardHeader>
                <CardTitle className="group-hover:underline">{k.name}</CardTitle>
                <CardDescription>Created {relative(k.created_at)}</CardDescription>
              </CardHeader>
              <CardContent className="grid grid-cols-3 gap-2 text-sm">
                <div>
                  <p className="text-lg font-semibold tabular-nums">
                    {k.ready_documents}
                    {k.documents !== k.ready_documents && <span className="text-sm font-normal text-muted-foreground">/{k.documents}</span>}
                  </p>
                  <p className="text-xs text-muted-foreground">documents</p>
                </div>
                <div>
                  <p className="text-lg font-semibold tabular-nums">{k.chunks}</p>
                  <p className="text-xs text-muted-foreground">chunks</p>
                </div>
                <div>
                  <p className="text-lg font-semibold tabular-nums">{bytes(k.size_bytes)}</p>
                  <p className="text-xs text-muted-foreground">uploaded</p>
                </div>
              </CardContent>
            </Card>
          </Link>
        ))}
      </div>
    </>
  );
}
