import { Suspense } from "react";

import { LoadingBlock } from "@/components/common";

import { AgentView } from "./view";

async function Content({ params }: { params: PageProps<"/agents/[id]">["params"] }) {
  const { id } = await params;
  return <AgentView id={id} />;
}

export default function Page({ params }: PageProps<"/agents/[id]">) {
  return (
    <Suspense fallback={<LoadingBlock rows={4} />}>
      <Content params={params} />
    </Suspense>
  );
}
