-- The connection to a PSA (ADR-0164, ADR-0166): at most one per
-- installation, managed by global administrators. The client secret is
-- sealed in `client_secret` (ADR-0125) and never returned by the API.
CREATE TABLE psa_connections (
    id              uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Always true: the unique index makes the table hold one row.
    single          boolean     NOT NULL DEFAULT true UNIQUE CHECK (single),
    kind            text        NOT NULL DEFAULT 'halopsa' CHECK (kind IN ('halopsa')),
    -- Where the PSA is, like https://halo.example.com, without /api.
    base_url        text        NOT NULL CHECK (base_url ~ '^https?://' AND char_length(base_url) <= 2048),
    -- Hosted Halo tenants name themselves on the token request.
    tenant          text        CHECK (char_length(tenant) BETWEEN 1 AND 200 AND tenant !~ '[[:cntrl:]]'),
    client_id       text        NOT NULL CHECK (char_length(client_id) BETWEEN 1 AND 200 AND client_id !~ '[[:cntrl:]]'),
    client_secret   text        NOT NULL CHECK (client_secret <> ''),
    enabled         boolean     NOT NULL DEFAULT true,
    -- The Halo agent the application acts as, from the last success.
    agent_name      text,
    last_success_at timestamptz,
    -- The last failure, in Halo's words when it gave some; NULL once a
    -- request works again.
    last_error      text,
    last_error_at   timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE psa_connections IS
    'The PSA connection; client_secret sealed (ADR-0125, ADR-0164, ADR-0166).';
