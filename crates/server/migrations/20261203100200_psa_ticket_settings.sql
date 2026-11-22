-- What tickets the PSA connection opens and how it closes them (ADR-0166):
-- the ticket type and category, chosen from the PSA's lists since some
-- instances refuse a ticket without a category; the least severity that
-- opens one; and the status a ticket moves to when its incident is
-- resolved, NULL for a note only.
ALTER TABLE psa_connections
    ADD COLUMN ticket_type_id     bigint CHECK (ticket_type_id > 0),
    ADD COLUMN ticket_type_name   text   CHECK (char_length(ticket_type_name) <= 500),
    ADD COLUMN category           text   CHECK (char_length(category) BETWEEN 1 AND 500),
    ADD COLUMN min_severity       text   NOT NULL DEFAULT 'critical'
                                         CHECK (min_severity IN ('warning', 'critical')),
    -- 9 is HaloPSA's built-in Closed.
    ADD COLUMN resolved_status_id bigint DEFAULT 9 CHECK (resolved_status_id > 0),
    ADD COLUMN resolved_status_name text DEFAULT 'Closed' CHECK (char_length(resolved_status_name) <= 500),
    ADD CHECK ((ticket_type_id IS NULL) = (ticket_type_name IS NULL)),
    ADD CHECK ((resolved_status_id IS NULL) = (resolved_status_name IS NULL));
