-- Agents: a reusable character for calls (prompt, details to collect,
-- knowledge base and call defaults), created and edited from the API/UI.
CREATE TABLE agents (
    id                 UUID PRIMARY KEY,
    tenant_id          UUID NOT NULL REFERENCES tenants (id),
    name               TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    description        TEXT NOT NULL DEFAULT '',
    prompt             TEXT NOT NULL CHECK (length(prompt) BETWEEN 1 AND 20000),
    greeting           TEXT,
    kb_id              UUID REFERENCES knowledge_bases (id) ON DELETE SET NULL,
    language           TEXT NOT NULL DEFAULT 'auto',
    voice              TEXT,
    booking            BOOLEAN NOT NULL DEFAULT false,
    max_duration_secs  INTEGER CHECK (max_duration_secs BETWEEN 10 AND 3600),
    -- [{key, label, description, type, options, required}]
    fields             JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX agents_tenant_idx ON agents (tenant_id, created_at DESC);

-- The agent a call used, and its field list at call time (later edits to
-- the agent do not change what an old call extracts).
ALTER TABLE calls
    ADD COLUMN agent_id UUID REFERENCES agents (id) ON DELETE SET NULL,
    ADD COLUMN collect_fields JSONB NOT NULL DEFAULT '[]'::jsonb;
CREATE INDEX calls_agent_idx ON calls (agent_id, created_at DESC);
