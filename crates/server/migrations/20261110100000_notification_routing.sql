-- Routing per customer and site (ADR-0078). A channel covers everything
-- (both columns NULL, as every existing channel without a customer stays),
-- one customer, or one site, like a grant in `user_roles`.
--
-- Removing a site removes its channels, as removing a customer always did:
-- SET NULL here would turn a site's channel into one for everything.
ALTER TABLE notification_channels
    ADD COLUMN site_id uuid REFERENCES sites (id) ON DELETE CASCADE,
    -- Set when someone other than a global administrator chose where this
    -- channel sends: delivery then reaches only public addresses, so the
    -- field cannot aim the central server at its own network.
    ADD COLUMN public_only boolean NOT NULL DEFAULT false,
    ADD CONSTRAINT notification_channels_one_scope
        CHECK (customer_id IS NULL OR site_id IS NULL);

-- The event's site, from the object its webhook payload carries: every
-- writer of the outbox is covered, and the two cannot disagree. NULL when
-- it names none, which only channels for its customer and for everything
-- then receive.
ALTER TABLE notification_outbox
    ADD COLUMN site_id uuid GENERATED ALWAYS AS ((details #>> '{site,id}')::uuid) STORED;
