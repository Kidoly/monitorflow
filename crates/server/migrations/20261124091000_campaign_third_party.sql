-- Which sources a campaign updates (ADR-0138): the OS, as every campaign
-- before did, and third-party managers as [{"manager", "packages"}].
ALTER TABLE campaigns
    ADD COLUMN include_os  boolean NOT NULL DEFAULT true,
    ADD COLUMN third_party jsonb   NOT NULL DEFAULT '[]'
        CHECK (jsonb_typeof(third_party) = 'array');
