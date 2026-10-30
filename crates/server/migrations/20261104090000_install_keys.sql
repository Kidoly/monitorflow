-- Reusable install keys per site (ADR-0058): one key enrolls many agents,
-- each under its own host name, until it expires, reaches its host limit
-- or is revoked. Only the SHA-256 of the secret is stored, as for tokens.
CREATE TABLE install_keys (
    id               uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    key_hash         bytea       NOT NULL UNIQUE CHECK (octet_length(key_hash) = 32),
    site_id          uuid        NOT NULL REFERENCES sites (id),
    created_by       uuid        REFERENCES users (id),
    created_at       timestamptz NOT NULL DEFAULT now(),
    expires_at       timestamptz NOT NULL,
    max_hosts        integer     CHECK (max_hosts > 0),
    host_count       integer     NOT NULL DEFAULT 0 CHECK (host_count >= 0),
    require_approval boolean     NOT NULL DEFAULT false,
    revoked_at       timestamptz,
    CHECK (expires_at > created_at),
    CHECK (max_hosts IS NULL OR host_count <= max_hosts)
);
COMMENT ON TABLE install_keys IS 'Reusable enrollment keys for agents, stored as SHA-256 hashes only.';
CREATE INDEX install_keys_site_id_idx ON install_keys (site_id);

-- Each enrollment through a key is recorded as a used single-use token
-- naming the host's ID, so everything that reads the site of an enrolled
-- agent from enrollment_tokens keeps working.
ALTER TABLE enrollment_tokens
    ADD COLUMN install_key_id uuid REFERENCES install_keys (id);
CREATE INDEX enrollment_tokens_install_key_idx ON enrollment_tokens (install_key_id, used_at)
    WHERE install_key_id IS NOT NULL;
COMMENT ON COLUMN enrollment_tokens.install_key_id IS
    'Install key that enrolled this peer; the row is then created used and its secret never existed.';

-- Agents enrolled through a key that asks for approval. While a row is
-- here the agent's stream is refused with UNAVAILABLE and it keeps
-- retrying; approving deletes the row, rejecting also revokes its
-- certificates.
CREATE TABLE pending_hosts (
    peer_id        text        PRIMARY KEY CHECK (peer_id ~ '^[A-Za-z0-9._-]{1,64}$'),
    site_id        uuid        NOT NULL REFERENCES sites (id),
    install_key_id uuid        NOT NULL REFERENCES install_keys (id),
    hostname       text        NOT NULL,
    source         text,
    created_at     timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE pending_hosts IS 'Agents enrolled by an install key and waiting for an administrator.';
CREATE INDEX pending_hosts_site_id_idx ON pending_hosts (site_id);
