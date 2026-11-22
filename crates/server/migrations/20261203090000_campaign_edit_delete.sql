-- Campaigns are edited until their first batch is queued and deleted
-- once over (ADR-0166).

-- What was asked, besides the frozen targets, so a campaign can be edited
-- or copied as it was written: the servers named and the batch
-- requests. NULL for campaigns made before.
ALTER TABLE campaigns
    ADD COLUMN requested_server_ids uuid[],
    ADD COLUMN requested_waves      jsonb CHECK (jsonb_typeof(requested_waves) = 'array');
COMMENT ON COLUMN campaigns.requested_server_ids IS
    'The server_ids of the request, besides site_id; NULL before ADR-0166.';
COMMENT ON COLUMN campaigns.requested_waves IS
    'The waves of the request, as the API takes them; NULL before ADR-0166.';

-- A deleted campaign leaves its commands in the history, without it.
ALTER TABLE commands
    DROP CONSTRAINT commands_campaign_id_fkey,
    ADD CONSTRAINT commands_campaign_id_fkey
        FOREIGN KEY (campaign_id) REFERENCES campaigns (id) ON DELETE SET NULL;
