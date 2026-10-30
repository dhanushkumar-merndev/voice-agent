-- Spend that belongs to no single call: phrase audio synthesized for the
-- shared cache (fixed lines, start-up warm-up) and "Edit with AI" prompt
-- edits. Per-call spend stays in calls.cost_inr / cost_breakdown.
CREATE TABLE spend_ledger (
    id          UUID PRIMARY KEY,
    tenant_id   UUID NOT NULL REFERENCES tenants (id),
    kind        TEXT NOT NULL CHECK (kind IN ('tts_cache', 'prompt_edit')),
    model       TEXT NOT NULL,
    -- Characters (TTS) or tokens (LLM).
    units       BIGINT NOT NULL DEFAULT 0 CHECK (units >= 0),
    cost_inr    DOUBLE PRECISION NOT NULL CHECK (cost_inr >= 0),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX spend_ledger_tenant_created ON spend_ledger (tenant_id, created_at);
