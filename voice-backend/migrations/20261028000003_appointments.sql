-- Appointments booked by the agent (book_appointment) or added by staff.
CREATE TABLE appointments (
    id             UUID PRIMARY KEY,
    tenant_id      UUID NOT NULL REFERENCES tenants (id),
    call_id        UUID REFERENCES calls (id) ON DELETE SET NULL,
    starts_at      TIMESTAMPTZ NOT NULL,
    duration_mins  INTEGER NOT NULL,
    name           TEXT NOT NULL,
    note           TEXT,
    status         TEXT NOT NULL DEFAULT 'booked',
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- One booking per slot; cancelled rows free the slot again.
CREATE UNIQUE INDEX appointments_slot ON appointments (tenant_id, starts_at) WHERE status = 'booked';
CREATE INDEX appointments_call ON appointments (call_id);
