-- Administrator rules of the risk classifier and the per-group
-- remediation policy (architecture §8, ADR-0041).
--
-- A rule belongs to one customer and applies to all its sites, or to one
-- site when site_id is set (sites are the server groups for now). The
-- API checks that the site belongs to the customer.
CREATE TABLE remediation_rules (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id uuid        NOT NULL REFERENCES customers (id),
    site_id     uuid        REFERENCES sites (id) ON DELETE CASCADE,
    -- allow: unknown commands matching the pattern get `tier`;
    -- deny: commands containing a match are forbidden;
    -- cache_dir: `pattern` is a directory whose contents may be cleared
    -- as a low-impact change.
    kind        text        NOT NULL CHECK (kind IN ('allow', 'deny', 'cache_dir')),
    pattern     text        NOT NULL CHECK (pattern <> '' AND octet_length(pattern) <= 512),
    tier        text        CHECK (tier IN ('read_only', 'low_impact')),
    description text        NOT NULL DEFAULT '' CHECK (char_length(description) <= 256),
    created_by  uuid        REFERENCES users (id),
    created_at  timestamptz NOT NULL DEFAULT now(),
    CHECK ((kind = 'allow') = (tier IS NOT NULL)),
    CONSTRAINT remediation_rules_key UNIQUE NULLS NOT DISTINCT (customer_id, site_id, kind, pattern)
);
COMMENT ON TABLE remediation_rules IS 'Allow, deny and cache directory rules of the risk classifier; see ADR-0041.';
CREATE INDEX remediation_rules_site_idx ON remediation_rules (site_id) WHERE site_id IS NOT NULL;

-- Whether low-impact commands run without approval. A site row overrides
-- the customer row (site_id NULL); without either they need approval.
CREATE TABLE remediation_policies (
    customer_id     uuid        NOT NULL REFERENCES customers (id),
    site_id         uuid        REFERENCES sites (id) ON DELETE CASCADE,
    low_impact_auto boolean     NOT NULL,
    updated_by      uuid        REFERENCES users (id),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT remediation_policies_key UNIQUE NULLS NOT DISTINCT (customer_id, site_id)
);
COMMENT ON TABLE remediation_policies IS 'Per customer or site remediation policy; see ADR-0041.';
