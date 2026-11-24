-- Software deployment and update approvals (ADR-0188).

-- Campaigns approve Windows Update classifications and hold updates
-- (KB numbers or update IDs); held apps live in each entry of
-- third_party as "hold". A campaign with `software` installs or removes
-- software from the catalogue instead of updating:
-- {"action", "packages": [{"catalog_id", "name", "manager", "id", "source"}]}.
ALTER TABLE campaigns
    ADD COLUMN classifications text[] NOT NULL DEFAULT '{}',
    ADD COLUMN hold            text[] NOT NULL DEFAULT '{}',
    ADD COLUMN software        jsonb
        CHECK (software IS NULL OR jsonb_typeof(software) = 'object');

-- The software servers can be given: shared by every customer when
-- customer_id is NULL, else one customer's own.
CREATE TABLE software_catalog (
    id          uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id uuid        REFERENCES customers (id) ON DELETE CASCADE,
    name        text        NOT NULL CHECK (char_length(name) BETWEEN 1 AND 128),
    manager     text        NOT NULL CHECK (manager IN ('winget')),
    package_id  text        NOT NULL CHECK (char_length(package_id) BETWEEN 1 AND 128),
    source      text        NOT NULL CHECK (char_length(source) BETWEEN 1 AND 64),
    description text        NOT NULL DEFAULT '' CHECK (char_length(description) <= 500),
    created_by  uuid        REFERENCES users (id) ON DELETE SET NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX software_catalog_package
    ON software_catalog (customer_id, manager, lower(package_id)) NULLS NOT DISTINCT;
COMMENT ON TABLE software_catalog IS
    'Software that campaigns and commands install or remove; see ADR-0188.';
