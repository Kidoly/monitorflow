-- Customers for MSP multi-tenancy (ADR-0026).
--
-- Every site belongs to one customer. Single-tenant installs never name
-- one: the migration creates the customer `default` with a fixed ID, moves
-- every existing site to it, and makes it the column default, so an
-- INSERT INTO sites without customer_id keeps working.
--
-- Site names stay unique across customers: the `site` label of
-- VictoriaMetrics series is the site name (ADR-0013), so two customers
-- with a site of the same name would share series.
CREATE TABLE customers (
    id         uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    name       text        NOT NULL UNIQUE CHECK (name <> ''),
    created_at timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE customers IS 'Tenants of an MSP install; each owns sites. See ADR-0026.';

INSERT INTO customers (id, name) VALUES ('00000000-0000-0000-0000-000000000001', 'default');

-- The default fills existing rows too, in the same statement.
ALTER TABLE sites
    ADD COLUMN customer_id uuid NOT NULL DEFAULT '00000000-0000-0000-0000-000000000001'
        REFERENCES customers (id);
COMMENT ON COLUMN sites.customer_id IS 'Owning customer; the default customer on single-tenant installs.';
CREATE INDEX sites_customer_id_idx ON sites (customer_id);

-- A grant is global (both NULL), on one customer and every site it has or
-- will have (customer_id), or on one site (site_id).
ALTER TABLE user_roles
    ADD COLUMN customer_id uuid REFERENCES customers (id) ON DELETE CASCADE,
    ADD CHECK (customer_id IS NULL OR site_id IS NULL),
    DROP CONSTRAINT user_roles_key,
    ADD CONSTRAINT user_roles_key UNIQUE NULLS NOT DISTINCT (user_id, role, customer_id, site_id);
COMMENT ON TABLE user_roles IS 'Role grants per user: global, on one customer, or on one site.';
CREATE INDEX user_roles_customer_id_idx ON user_roles (customer_id) WHERE customer_id IS NOT NULL;
