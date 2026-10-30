-- Full cost per call: AI services split by service, plus the phone charge
-- from Plivo's call detail record (fetched shortly after hangup).
ALTER TABLE calls
    ADD COLUMN cost_breakdown             JSONB NOT NULL DEFAULT '{}'::jsonb,  -- {stt, tts, llm, extraction} in ₹
    ADD COLUMN telephony_cost_inr         DOUBLE PRECISION,
    ADD COLUMN telephony_billed_secs      INTEGER,
    ADD COLUMN telephony_rate_inr_per_min DOUBLE PRECISION;
