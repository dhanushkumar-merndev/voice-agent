-- What the realtime STT heard, kept when the post-call batch STT replaces `text`.
ALTER TABLE call_turns ADD COLUMN live_text TEXT;
