-- A person's own preferences for the web interface (ADR-0137): one small
-- JSON object per user, its shape checked by the API. Removing the user
-- removes them.
CREATE TABLE user_preferences (
    user_id    uuid        PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    document   jsonb       NOT NULL CHECK (jsonb_typeof(document) = 'object'
                                           AND octet_length(document::text) <= 16384),
    updated_at timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE user_preferences IS 'Per-user web interface preferences; see ADR-0137.';
