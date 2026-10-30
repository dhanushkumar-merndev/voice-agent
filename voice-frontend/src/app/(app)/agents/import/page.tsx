import { ImportPack } from "@/components/agent-pack";
import { PageHeader } from "@/components/common";

export const metadata = { title: "Import agent pack" };

export default function ImportPackPage() {
  return (
    <>
      <PageHeader
        title="Import an agent pack"
        description="Let ChatGPT or Claude turn your business files into an agent and its knowledge base, then upload the ZIP here."
      />
      <ImportPack />
    </>
  );
}
