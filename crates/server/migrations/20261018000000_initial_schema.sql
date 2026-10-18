-- Initial schema: sites, peers, enrollment, certificates, users and audit.
--
-- Enumerated values are `text` columns with CHECK constraints rather than
-- PostgreSQL enums: a new value is a DROP/ADD CONSTRAINT in one migration,
-- usable in the same transaction, and a value can be removed later.
--
-- Peer IDs follow `common::tls::validate_id`: 1 to 64 characters from
-- [A-Za-z0-9._-]. The same CHECK is repeated on every peer ID column.

-- A customer location or network. Relays and servers belong to one site.
CREATE TABLE sites (
    id         uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    name       text        NOT NULL UNIQUE CHECK (name <> ''),
    created_at timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE sites IS 'Customer location or network grouping relays and servers.';

-- A relay forwards envelopes for the agents of its site (ADR-0004).
CREATE TABLE relays (
    id           uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    peer_id      text        NOT NULL UNIQUE CHECK (peer_id ~ '^[A-Za-z0-9._-]{1,64}$'),
    site_id      uuid        NOT NULL REFERENCES sites (id),
    name         text        NOT NULL CHECK (name <> ''),
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz,
    revoked_at   timestamptz,
    -- Target of the composite foreign key on servers (relay_id, site_id).
    UNIQUE (id, site_id)
);
COMMENT ON TABLE relays IS 'Enrolled relays; peer_id is the ID in the certificate URI SAN.';
CREATE INDEX relays_site_id_idx ON relays (site_id);

-- A managed machine running an agent.
CREATE TABLE servers (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    peer_id       text        NOT NULL UNIQUE CHECK (peer_id ~ '^[A-Za-z0-9._-]{1,64}$'),
    site_id       uuid        NOT NULL REFERENCES sites (id),
    relay_id      uuid,
    hostname      text        NOT NULL CHECK (hostname <> ''),
    os_family     text        NOT NULL CHECK (os_family IN ('debian', 'rhel')),
    os_version    text,
    agent_version text,
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_seen_at  timestamptz,
    revoked_at    timestamptz,
    -- The assigned relay must belong to the same site. MATCH SIMPLE skips
    -- the check when relay_id is NULL.
    FOREIGN KEY (relay_id, site_id) REFERENCES relays (id, site_id)
);
COMMENT ON TABLE servers IS 'Managed machines running an agent; peer_id is the agent certificate ID.';
CREATE INDEX servers_site_id_idx ON servers (site_id);
CREATE INDEX servers_relay_id_idx ON servers (relay_id) WHERE relay_id IS NOT NULL;

-- Web interface accounts. Hashing is done by the application (argon2).
CREATE TABLE users (
    id            uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    username      text        NOT NULL CHECK (username <> ''),
    password_hash text        NOT NULL CHECK (password_hash LIKE '$argon2%'),
    created_at    timestamptz NOT NULL DEFAULT now(),
    disabled_at   timestamptz
);
COMMENT ON TABLE users IS 'Web interface accounts; password_hash is an argon2 PHC string.';
-- Usernames are unique regardless of case.
CREATE UNIQUE INDEX users_username_lower_key ON users (lower(username));

-- Role grants. A NULL site_id means the role applies to every site.
-- A primary key cannot include the nullable site_id, so uniqueness over the
-- three columns uses NULLS NOT DISTINCT (PostgreSQL 15+).
CREATE TABLE user_roles (
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role    text NOT NULL CHECK (role IN ('viewer', 'operator', 'approver', 'administrator')),
    site_id uuid REFERENCES sites (id) ON DELETE CASCADE,
    CONSTRAINT user_roles_key UNIQUE NULLS NOT DISTINCT (user_id, role, site_id)
);
COMMENT ON TABLE user_roles IS 'Role grants per user, global (site_id NULL) or scoped to one site.';
CREATE INDEX user_roles_site_id_idx ON user_roles (site_id) WHERE site_id IS NOT NULL;

-- One-time enrollment tokens. Only the SHA-256 of the token is stored.
--
-- Single use: used_at and used_by_peer_id are set together, and the
-- enrollment_tokens_single_use trigger rejects any UPDATE of a row whose
-- used_at is already set. Consumers should still filter on
-- `used_at IS NULL AND expires_at > now()` in the UPDATE that consumes it.
CREATE TABLE enrollment_tokens (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    token_hash      bytea       NOT NULL UNIQUE CHECK (octet_length(token_hash) = 32),
    role            text        NOT NULL CHECK (role IN ('agent', 'relay')),
    site_id         uuid        NOT NULL REFERENCES sites (id),
    created_by      uuid        NOT NULL REFERENCES users (id),
    created_at      timestamptz NOT NULL DEFAULT now(),
    expires_at      timestamptz NOT NULL,
    used_at         timestamptz,
    used_by_peer_id text        CHECK (used_by_peer_id ~ '^[A-Za-z0-9._-]{1,64}$'),
    CHECK (expires_at > created_at),
    CHECK ((used_at IS NULL) = (used_by_peer_id IS NULL))
);
COMMENT ON TABLE enrollment_tokens IS 'Single-use enrollment tokens, stored as SHA-256 hashes only.';
CREATE INDEX enrollment_tokens_site_id_idx ON enrollment_tokens (site_id);
CREATE INDEX enrollment_tokens_created_by_idx ON enrollment_tokens (created_by);

CREATE FUNCTION enrollment_tokens_single_use() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.used_at IS NOT NULL THEN
        RAISE EXCEPTION 'enrollment token % was already used', OLD.id
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER enrollment_tokens_single_use
    BEFORE UPDATE ON enrollment_tokens
    FOR EACH ROW EXECUTE FUNCTION enrollment_tokens_single_use();

-- Certificates issued by the built-in CA. peer_id has no foreign key: it
-- points to relays, servers or the central server depending on peer_role.
CREATE TABLE certificates (
    serial            bytea       PRIMARY KEY CHECK (octet_length(serial) BETWEEN 1 AND 20),
    peer_role         text        NOT NULL CHECK (peer_role IN ('agent', 'relay', 'server')),
    peer_id           text        NOT NULL CHECK (peer_id ~ '^[A-Za-z0-9._-]{1,64}$'),
    not_before        timestamptz NOT NULL,
    not_after         timestamptz NOT NULL,
    issued_at         timestamptz NOT NULL DEFAULT now(),
    revoked_at        timestamptz,
    revocation_reason text,
    CHECK (not_after > not_before),
    CHECK (revocation_reason IS NULL OR revoked_at IS NOT NULL)
);
COMMENT ON TABLE certificates IS 'Certificates issued by the CA, with revocation state for the handshake check.';
-- "Active certificates of peer X": unrevoked, filtered on not_after.
CREATE INDEX certificates_active_peer_idx ON certificates (peer_role, peer_id, not_after)
    WHERE revoked_at IS NULL;

-- Append-only record of every action taken by users, the system, the AI or
-- peers. The triggers below reject UPDATE, DELETE and TRUNCATE.
-- Production must also grant the application role only INSERT and SELECT on
-- this table (plus USAGE on its sequence): a table owner or superuser can
-- disable triggers.
CREATE TABLE audit_log (
    id          bigserial   PRIMARY KEY,
    occurred_at timestamptz NOT NULL DEFAULT now(),
    actor_kind  text        NOT NULL CHECK (actor_kind IN ('user', 'system', 'ai', 'peer')),
    actor_id    text        NOT NULL,
    action      text        NOT NULL CHECK (action <> ''),
    target_kind text,
    target_id   text,
    details     jsonb       NOT NULL DEFAULT '{}'::jsonb
);
COMMENT ON TABLE audit_log IS 'Append-only audit trail. Production grants the app role INSERT and SELECT only.';
CREATE INDEX audit_log_occurred_at_idx ON audit_log (occurred_at);
CREATE INDEX audit_log_target_idx ON audit_log (target_kind, target_id, occurred_at)
    WHERE target_kind IS NOT NULL;

CREATE FUNCTION audit_log_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'audit_log is append-only: % is not allowed', TG_OP
        USING ERRCODE = 'insufficient_privilege';
END;
$$;

CREATE TRIGGER audit_log_no_update_delete
    BEFORE UPDATE OR DELETE ON audit_log
    FOR EACH ROW EXECUTE FUNCTION audit_log_append_only();

CREATE TRIGGER audit_log_no_truncate
    BEFORE TRUNCATE ON audit_log
    FOR EACH STATEMENT EXECUTE FUNCTION audit_log_append_only();
