import { Suspense } from "react";

import { LoadingBlock } from "@/components/common";

import { CallView } from "./view";

async function Content({ params }: { params: PageProps<"/calls/[id]">["params"] }) {
  const { id } = await params;
  return <CallView id={id} />;
}

export default function Page({ params }: PageProps<"/calls/[id]">) {
  return (
    <Suspense fallback={<LoadingBlock rows={5} />}>
      <Content params={params} />
    </Suspense>
  );
}
