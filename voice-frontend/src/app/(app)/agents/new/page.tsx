import { AgentForm } from "@/components/agent-form";
import { PageHeader } from "@/components/common";

export const metadata = { title: "New agent" };

export default function NewAgentPage() {
  return (
    <>
      <PageHeader title="New agent" description="A character for your calls, with its own prompt, details to collect and knowledge base." />
      <AgentForm />
    </>
  );
}
