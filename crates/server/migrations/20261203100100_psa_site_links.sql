-- Which HaloPSA client, and optionally which of its sites, a MonitorFlow
-- site's incidents open tickets for (ADR-0164, ADR-0166). Ids are what is
-- used; names are kept only to show what was matched.
CREATE TABLE psa_site_links (
    site_id        uuid        PRIMARY KEY REFERENCES sites (id) ON DELETE CASCADE,
    connection_id  uuid        NOT NULL REFERENCES psa_connections (id) ON DELETE CASCADE,
    client_id      bigint      NOT NULL CHECK (client_id > 0),
    client_name    text        NOT NULL CHECK (char_length(client_name) <= 500),
    halo_site_id   bigint      CHECK (halo_site_id > 0),
    halo_site_name text        CHECK (char_length(halo_site_name) <= 500),
    linked_at      timestamptz NOT NULL DEFAULT now(),
    CHECK ((halo_site_id IS NULL) = (halo_site_name IS NULL))
);
-- One Halo site at most one MonitorFlow site; a client may have several.
CREATE UNIQUE INDEX psa_site_links_halo_site_idx ON psa_site_links (connection_id, halo_site_id)
    WHERE halo_site_id IS NOT NULL;
COMMENT ON TABLE psa_site_links IS
    'MonitorFlow sites matched to PSA clients and sites (ADR-0164, ADR-0166).';
