-- Phones (ADR-0186): the installed web app on a person's phone, with its
-- long session and its Web Push subscription.

-- The server's own VAPID key pair (RFC 8292), made on first use. One row;
-- the private key is a PKCS#8 document in base64, sealed (ADR-0125).
CREATE TABLE web_push_keys (
    id          smallint    PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    public_key  bytea       NOT NULL CHECK (octet_length(public_key) = 65),
    private_key text        NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE web_push_keys IS
    'The VAPID key pair push services know this server by (ADR-0186).';

-- One phone of one person. Its push subscription is optional: a phone
-- that refused notifications still keeps its long session.
CREATE TABLE phones (
    id             uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id        uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name           text        NOT NULL CHECK (char_length(name) BETWEEN 1 AND 80),
    created_at     timestamptz NOT NULL DEFAULT now(),
    last_seen_at   timestamptz NOT NULL DEFAULT now(),
    -- The browser's push subscription (W3C Push API): where to post, and
    -- the keys messages are encrypted to (RFC 8291).
    push_endpoint  text        UNIQUE CHECK (char_length(push_endpoint) <= 2048),
    push_p256dh    bytea       CHECK (octet_length(push_p256dh) = 65),
    push_auth      bytea       CHECK (octet_length(push_auth) = 16),
    push_sent_at   timestamptz,
    push_error     text,
    CHECK ((push_endpoint IS NULL) = (push_p256dh IS NULL)
           AND (push_endpoint IS NULL) = (push_auth IS NULL))
);
COMMENT ON TABLE phones IS
    'A person''s phones with the web app installed, and their push subscriptions (ADR-0186).';
CREATE INDEX phones_user_id_idx ON phones (user_id);

-- A session opened on a phone lasts longer and ends with it.
ALTER TABLE sessions
    ADD COLUMN phone_id uuid REFERENCES phones (id) ON DELETE CASCADE;
CREATE INDEX sessions_phone_id_idx ON sessions (phone_id) WHERE phone_id IS NOT NULL;
