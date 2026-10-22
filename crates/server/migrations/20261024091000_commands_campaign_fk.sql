-- ADR-0031 left commands.campaign_id without a foreign key until
-- campaigns existed (ADR-0033). Campaigns are never deleted.
ALTER TABLE commands
    ADD CONSTRAINT commands_campaign_id_fkey FOREIGN KEY (campaign_id) REFERENCES campaigns (id);
