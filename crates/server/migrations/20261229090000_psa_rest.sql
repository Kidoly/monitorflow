-- The rest of the PSA connection (ADR-0164, ADR-0191): customers matched
-- as well as sites, a default client, the priority map, ticket statuses
-- read back from the PSA, silences holding tickets, and servers synced as
-- assets.

ALTER TABLE psa_connections
    -- The PSA agent the application acts as: tickets assigned to it by the
    -- ticket type's defaults are not someone taking them.
    ADD COLUMN agent_id             bigint,
    -- The ticket statuses that mean "closed" (9 is HaloPSA's built-in
    -- Closed): a ticket moved to one acknowledges its open incident.
    ADD COLUMN closed_status_ids    bigint[] NOT NULL DEFAULT '{9}'
                                    CHECK (cardinality(closed_status_ids) <= 20),
    -- Whether a ticket assigned to an agent acknowledges its incident.
    ADD COLUMN acknowledge_on_assign boolean NOT NULL DEFAULT true,
    -- The priority of new tickets per severity: a priority of the ticket
    -- type's SLA, NULL for the PSA's default.
    ADD COLUMN warning_priority_id   bigint CHECK (warning_priority_id > 0),
    ADD COLUMN warning_priority_name text   CHECK (char_length(warning_priority_name) <= 500),
    ADD COLUMN critical_priority_id  bigint CHECK (critical_priority_id > 0),
    ADD COLUMN critical_priority_name text  CHECK (char_length(critical_priority_name) <= 500),
    -- Where incidents of a site matched to nothing open tickets; NULL for
    -- no ticket.
    ADD COLUMN default_client_id     bigint CHECK (default_client_id > 0),
    ADD COLUMN default_client_name   text   CHECK (char_length(default_client_name) <= 500),
    ADD COLUMN default_site_id       bigint CHECK (default_site_id > 0),
    ADD COLUMN default_site_name     text   CHECK (char_length(default_site_name) <= 500),
    -- Servers of matched sites and customers synced as PSA assets.
    ADD COLUMN asset_sync            boolean NOT NULL DEFAULT false,
    -- Whether a removed server's asset is marked inactive.
    ADD COLUMN asset_inactive_on_removal boolean NOT NULL DEFAULT false,
    ADD CHECK ((warning_priority_id IS NULL) = (warning_priority_name IS NULL)),
    ADD CHECK ((critical_priority_id IS NULL) = (critical_priority_name IS NULL)),
    ADD CHECK ((default_client_id IS NULL) = (default_client_name IS NULL)),
    ADD CHECK ((default_site_id IS NULL) = (default_site_name IS NULL)),
    ADD CHECK (default_site_id IS NULL OR default_client_id IS NOT NULL);

-- Which PSA client a MonitorFlow customer is: its sites without a link of
-- their own open tickets for it, and a site's link must name it.
CREATE TABLE psa_customer_links (
    customer_id   uuid        PRIMARY KEY REFERENCES customers (id) ON DELETE CASCADE,
    connection_id uuid        NOT NULL REFERENCES psa_connections (id) ON DELETE CASCADE,
    client_id     bigint      NOT NULL CHECK (client_id > 0),
    client_name   text        NOT NULL CHECK (char_length(client_name) <= 500),
    linked_at     timestamptz NOT NULL DEFAULT now(),
    -- One PSA client at most one customer.
    UNIQUE (connection_id, client_id)
);
COMMENT ON TABLE psa_customer_links IS
    'MonitorFlow customers matched to PSA clients (ADR-0164, ADR-0191).';

-- held: a silence holds the ticket until it ends; ended: the ticket was
-- deleted or merged in the PSA, so the link ends.
ALTER TABLE psa_tickets DROP CONSTRAINT psa_tickets_state_check;
ALTER TABLE psa_tickets ADD CONSTRAINT psa_tickets_state_check
    CHECK (state IN ('creating', 'open', 'closed', 'skipped', 'failed', 'held', 'ended'));
ALTER TABLE psa_tickets
    -- When the poller last read the ticket from the PSA.
    ADD COLUMN checked_at    timestamptz,
    -- What the PSA said then.
    ADD COLUMN psa_status_id bigint,
    ADD COLUMN psa_agent_id  bigint;
-- The poller reads the tickets checked longest ago first.
CREATE INDEX psa_tickets_poll_idx ON psa_tickets (checked_at NULLS FIRST)
    WHERE state IN ('open', 'closed');

-- An incident acknowledged from the PSA: a ticket closed or taken there.
ALTER TABLE incident_acknowledgements DROP CONSTRAINT incident_acknowledgements_via_check;
ALTER TABLE incident_acknowledgements ADD CONSTRAINT incident_acknowledgements_via_check
    CHECK (via IN ('page', 'link', 'psa'));

-- The PSA asset type of each system family's servers, and which of the
-- type's fields receive the host name, serial number, maker and model:
-- `inventory_number` (HaloPSA's asset tag) or a field id; NULL for none.
CREATE TABLE psa_asset_types (
    connection_id      uuid   NOT NULL REFERENCES psa_connections (id) ON DELETE CASCADE,
    family             text   NOT NULL CHECK (family IN ('linux', 'windows')),
    type_id            bigint NOT NULL CHECK (type_id > 0),
    type_name          text   NOT NULL CHECK (char_length(type_name) <= 500),
    hostname_field     text   CHECK (hostname_field ~ '^(inventory_number|[1-9][0-9]{0,17})$'),
    serial_field       text   CHECK (serial_field ~ '^(inventory_number|[1-9][0-9]{0,17})$'),
    manufacturer_field text   CHECK (manufacturer_field ~ '^(inventory_number|[1-9][0-9]{0,17})$'),
    model_field        text   CHECK (model_field ~ '^(inventory_number|[1-9][0-9]{0,17})$'),
    PRIMARY KEY (connection_id, family)
);
COMMENT ON TABLE psa_asset_types IS
    'PSA asset type and fields per system family (ADR-0164, ADR-0191).';

-- A server and its PSA asset. `proposed` is an existing asset found by
-- serial number or host name, waiting for an administrator to confirm it;
-- `create` makes a new asset at the next pass, as an administrator chose;
-- `linked` is synced; `failed` says why the PSA refused.
CREATE TABLE psa_asset_links (
    server_id     uuid        PRIMARY KEY REFERENCES servers (id) ON DELETE CASCADE,
    connection_id uuid        NOT NULL REFERENCES psa_connections (id) ON DELETE CASCADE,
    asset_id      bigint      CHECK (asset_id > 0),
    state         text        NOT NULL CHECK (state IN ('proposed', 'create', 'linked', 'failed')),
    -- How the asset was found: serial, hostname, created, or chosen by an
    -- administrator.
    matched_by    text        CHECK (matched_by IN ('serial', 'hostname', 'created', 'chosen')),
    -- The asset's key field when found, to show what was matched.
    asset_name    text        CHECK (char_length(asset_name) <= 500),
    -- SHA-256, hex, of what was last sent, so only changes are pushed.
    pushed_hash   text,
    pushed_at     timestamptz,
    -- Whether the asset was marked inactive after the server's removal.
    deactivated   boolean     NOT NULL DEFAULT false,
    error         text,
    updated_at    timestamptz NOT NULL DEFAULT now(),
    CHECK ((state IN ('proposed', 'linked')) <= (asset_id IS NOT NULL))
);
CREATE UNIQUE INDEX psa_asset_links_asset_idx ON psa_asset_links (connection_id, asset_id)
    WHERE asset_id IS NOT NULL;
COMMENT ON TABLE psa_asset_links IS
    'Servers synced as PSA assets (ADR-0164, ADR-0191).';
