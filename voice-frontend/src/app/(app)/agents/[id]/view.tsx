"use client";

import { AgentForm } from "@/components/agent-form";
import { ExportPackButton } from "@/components/agent-pack";
import { ErrorAlert, LoadingBlock, PageHeader } from "@/components/common";
import { AgentTraining } from "@/components/training";
import { useApi } from "@/hooks/use-api";
import type { Agent } from "@/lib/types";

export function AgentView({ id }: { id: string }) {
  const agent = useApi<Agent>(`agents/${id}`);
  if (agent.loading) return <LoadingBlock rows={4} />;
  if (!agent.data) return <ErrorAlert error={agent.error ?? "Agent not found"} title="Could not load the agent" />;
  return (
    <>
      <PageHeader title={agent.data.name} description="Changes apply to new calls; past calls keep what they used."
        actions={<ExportPackButton agent={agent.data} />}
      />
      <AgentForm key={agent.data.updated_at} agent={agent.data} />
      <AgentTraining agentId={id} />
    </>
  );
}
