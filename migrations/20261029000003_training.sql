-- Learning from calls: each agent has training notes (its train.md), added
-- to its prompt on every call. After a call a reviewer scores it, lists the
-- agent's mistakes and suggests lessons; approved lessons are appended to
-- the notes. People can also rate a call and say what went wrong.
ALTER TABLE agents
    ADD COLUMN training_notes TEXT NOT NULL DEFAULT '' CHECK (length(training_notes) <= 8000),
    -- Append suggested lessons to the notes without waiting for approval.
    ADD COLUMN auto_apply_lessons BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE calls
    -- {score, went_well[], mistakes[{turn, kind, what}], model, reviewed_at}
    ADD COLUMN review JSONB,
    ADD COLUMN review_score SMALLINT CHECK (review_score BETWEEN 1 AND 10),
    -- A person's verdict: 1 good, -1 bad, with an optional note.
    ADD COLUMN rating SMALLINT CHECK (rating IN (-1, 1)),
    ADD COLUMN rating_note TEXT CHECK (length(rating_note) <= 500);

CREATE TABLE agent_lessons (
    id          UUID PRIMARY KEY,
    tenant_id   UUID NOT NULL REFERENCES tenants (id),
    agent_id    UUID NOT NULL REFERENCES agents (id) ON DELETE CASCADE,
    call_id     UUID REFERENCES calls (id) ON DELETE SET NULL,
    -- The instruction the agent will follow.
    lesson      TEXT NOT NULL CHECK (length(lesson) BETWEEN 3 AND 300),
    -- What went wrong on the call that prompted it.
    mistake     TEXT NOT NULL DEFAULT '' CHECK (length(mistake) <= 500),
    source      TEXT NOT NULL CHECK (source IN ('review', 'feedback')),
    status      TEXT NOT NULL DEFAULT 'suggested' CHECK (status IN ('suggested', 'applied', 'rejected')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    decided_at  TIMESTAMPTZ
);
CREATE INDEX agent_lessons_agent_idx ON agent_lessons (agent_id, status, created_at DESC);
