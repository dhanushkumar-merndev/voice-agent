import { Suspense } from "react";

import { LoadingBlock } from "@/components/common";

import { KnowledgeBaseView } from "./view";

async function Content({ params }: { params: PageProps<"/knowledge/[id]">["params"] }) {
  const { id } = await params;
  return <KnowledgeBaseView id={id} />;
}

export default function Page({ params }: PageProps<"/knowledge/[id]">) {
  return (
    <Suspense fallback={<LoadingBlock rows={4} />}>
      <Content params={params} />
    </Suspense>
  );
}
