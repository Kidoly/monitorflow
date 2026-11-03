-- Dashboards (ADR-0095): pages of panels a user builds by picking from
-- lists, kept for one user, or shared with a site or a customer. The
-- panels are a versioned JSON spec the API validates on every write.

CREATE TABLE dashboards (
    id             uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    name           text        NOT NULL CHECK (name <> '' AND char_length(name) <= 80),
    -- Who it is for: exactly one of these.
    owner_id       uuid        REFERENCES users (id) ON DELETE CASCADE,
    site_id        uuid        REFERENCES sites (id) ON DELETE CASCADE,
    customer_id    uuid        REFERENCES customers (id) ON DELETE CASCADE,
    schema_version integer     NOT NULL CHECK (schema_version > 0),
    spec           jsonb       NOT NULL CHECK (jsonb_typeof(spec) = 'object'),
    -- A save names the revision it started from; another save in between
    -- makes it fail instead of overwriting.
    revision       bigint      NOT NULL DEFAULT 1,
    created_by     uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_by     uuid        REFERENCES users (id) ON DELETE SET NULL,
    updated_at     timestamptz NOT NULL DEFAULT now(),
    CHECK (num_nonnulls(owner_id, site_id, customer_id) = 1)
);
COMMENT ON TABLE dashboards IS
    'User-built pages of charts, figures, top lists and alerts. See ADR-0095.';
CREATE INDEX dashboards_owner_id_idx ON dashboards (owner_id);
CREATE INDEX dashboards_site_id_idx ON dashboards (site_id);
CREATE INDEX dashboards_customer_id_idx ON dashboards (customer_id);
