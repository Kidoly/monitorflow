-- The first Hello of an agent looks up the site of the latest token it
-- consumed (ADR-0008), and so does the relay policy for agents without a
-- servers row yet.
CREATE INDEX enrollment_tokens_used_peer_idx ON enrollment_tokens (peer_id, used_at)
    WHERE used_at IS NOT NULL;
