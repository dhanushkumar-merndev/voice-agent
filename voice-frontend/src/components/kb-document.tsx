"use client";

import { useState } from "react";
import { DownloadIcon } from "lucide-react";
import { toast } from "sonner";

import { ErrorAlert, LoadingBlock } from "@/components/common";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { useApi } from "@/hooks/use-api";
import { apiGet, errorMessage } from "@/lib/api";
import { bytes, relative } from "@/lib/format";
import type { KbDocument, KbDocumentDetail } from "@/lib/types";

/** Side panel showing one knowledge-base document: its text and the chunks the agent searches. */
export function DocumentSheet({ kbId, doc, onClose }: { kbId: string; doc: KbDocument | null; onClose: () => void }) {
  return (
    <Sheet open={doc !== null} onOpenChange={(open) => !open && onClose()}>
      <SheetContent side="right" className="gap-0 data-[side=right]:w-full data-[side=right]:sm:max-w-2xl">
        {doc && <DocumentBody kbId={kbId} doc={doc} />}
      </SheetContent>
    </Sheet>
  );
}

function DocumentBody({ kbId, doc }: { kbId: string; doc: KbDocument }) {
  const path = `knowledge-bases/${kbId}/documents/${doc.id}`;
  const detail = useApi<KbDocumentDetail>(path);
  const [opening, setOpening] = useState(false);
  const d = detail.data;

  // Signed links last 5 minutes, so fetch a fresh one on each click.
  async function openOriginal() {
    setOpening(true);
    try {
      const fresh = await apiGet<KbDocumentDetail>(path);
      if (!fresh.file_url) throw new Error("The original file is not stored for this document.");
      window.open(fresh.file_url, "_blank", "noopener");
    } catch (e) {
      toast.error("Could not open the original file", { description: errorMessage(e) });
    } finally {
      setOpening(false);
    }
  }

  return (
    <>
      <SheetHeader className="border-b">
        <SheetTitle className="truncate pr-8">{doc.filename}</SheetTitle>
        <SheetDescription>
          {bytes(doc.size_bytes)} · {doc.chunks} chunk{doc.chunks === 1 ? "" : "s"} · added {relative(doc.created_at)}
        </SheetDescription>
        <div>
          <Button variant="outline" size="sm" disabled={opening} onClick={openOriginal}>
            <DownloadIcon data-icon="inline-start" />
            Original file
          </Button>
        </div>
      </SheetHeader>
      <div className="min-h-0 min-w-0 flex-1 overflow-x-hidden overflow-y-auto p-4">
        {detail.error && !d && <ErrorAlert error={detail.error} title="Could not load the document" />}
        {detail.loading && <LoadingBlock rows={6} />}
        {d && (
          <Tabs defaultValue={d.text !== null ? "text" : "chunks"}>
            <TabsList className="max-w-full">
              <TabsTrigger value="text">Document</TabsTrigger>
              <TabsTrigger value="chunks">What the agent searches ({d.chunks.length})</TabsTrigger>
            </TabsList>
            <TabsContent value="text" className="mt-3">
              {d.text !== null ? (
                <pre className="rounded-lg bg-muted p-4 font-sans text-sm leading-relaxed [overflow-wrap:anywhere] whitespace-pre-wrap">{d.text}</pre>
              ) : (
                <p className="text-sm text-muted-foreground">
                  Only Markdown and text files are shown here. Use <span className="font-medium text-foreground">Original file</span> to open
                  this one, or see the text the agent searches in the other tab.
                </p>
              )}
            </TabsContent>
            <TabsContent value="chunks" className="mt-3">
              {d.chunks.length === 0 ? (
                <p className="text-sm text-muted-foreground">{d.status === "ready" ? "This document has no text." : "Not split into chunks yet."}</p>
              ) : (
                <>
                  <p className="mb-3 text-xs text-muted-foreground">
                    On a call the agent finds the closest chunks to the caller&apos;s question. Neighbouring chunks overlap a little so no
                    sentence is cut in half.
                  </p>
                  <ol className="grid gap-3">
                    {d.chunks.map((c) => (
                      <li key={c.seq} className="min-w-0 rounded-lg border p-3">
                        <div className="mb-1.5 flex items-center gap-2 text-xs text-muted-foreground">
                          <Badge variant="secondary" className="font-normal tabular-nums">
                            #{c.seq + 1}
                          </Badge>
                          <span className="tabular-nums">{c.tokens} tokens</span>
                        </div>
                        <p className="text-sm [overflow-wrap:anywhere] whitespace-pre-wrap">{c.content}</p>
                      </li>
                    ))}
                  </ol>
                </>
              )}
            </TabsContent>
          </Tabs>
        )}
      </div>
    </>
  );
}
