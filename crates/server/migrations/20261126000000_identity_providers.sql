-- Signing in through one's own identity provider (ADR-0145): OpenID
-- Connect providers and LDAP directories, the groups that give roles, and
-- the accounts they create.
CREATE TABLE identity_providers (
    id         uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    kind       text        NOT NULL CHECK (kind IN ('oidc', 'ldap')),
    -- What the login page's button and the Users page say.
    name       text        NOT NULL CHECK (name <> '' AND length(name) <= 64),
    enabled    boolean     NOT NULL DEFAULT false,
    -- The settings of its kind, without the secret.
    config     jsonb       NOT NULL,
    -- The client secret or the service account's password, sealed with the
    -- secrets key (ADR-0125); never returned by the API.
    secret     text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX identity_providers_name_key ON identity_providers (lower(name));
COMMENT ON TABLE identity_providers IS
    'OpenID Connect providers and LDAP directories people sign in through; see ADR-0145.';

-- A group of a provider gives a role, on everything, a customer or a site,
-- like a grant of user_roles (ADR-0026).
CREATE TABLE identity_provider_groups (
    id          uuid NOT NULL PRIMARY KEY DEFAULT gen_random_uuid(),
    provider_id uuid NOT NULL REFERENCES identity_providers (id) ON DELETE CASCADE,
    group_name  text NOT NULL CHECK (group_name <> '' AND length(group_name) <= 256),
    role        text NOT NULL CHECK (role IN ('viewer', 'operator', 'approver', 'administrator')),
    customer_id uuid REFERENCES customers (id) ON DELETE CASCADE,
    site_id     uuid REFERENCES sites (id) ON DELETE CASCADE,
    CHECK (customer_id IS NULL OR site_id IS NULL)
);
CREATE UNIQUE INDEX identity_provider_groups_key ON identity_provider_groups
    (provider_id, lower(group_name), role, customer_id, site_id) NULLS NOT DISTINCT;
COMMENT ON TABLE identity_provider_groups IS
    'Which role each group of a provider gives; see ADR-0145.';

-- An account a provider created: never a password, one identity per
-- provider. Removing the provider keeps the account without one.
ALTER TABLE users
    ADD COLUMN identity_provider_id uuid
        REFERENCES identity_providers (id) ON DELETE SET NULL,
    ADD COLUMN external_subject text,
    ADD COLUMN external_groups  text[],
    ADD CONSTRAINT users_external_has_no_password
        CHECK (identity_provider_id IS NULL OR password_hash IS NULL),
    ADD CONSTRAINT users_external_subject_needs_provider
        CHECK (external_subject IS NULL OR identity_provider_id IS NOT NULL);
CREATE UNIQUE INDEX users_external_identity_key
    ON users (identity_provider_id, external_subject)
    WHERE identity_provider_id IS NOT NULL;
COMMENT ON COLUMN users.identity_provider_id IS
    'The provider the account signs in through, NULL for a local account; see ADR-0145.';
COMMENT ON COLUMN users.external_subject IS
    'The OpenID Connect sub, or the directory username in lower case.';
COMMENT ON COLUMN users.external_groups IS
    'The groups the provider gave at the last sign-in, for mapping changes.';
