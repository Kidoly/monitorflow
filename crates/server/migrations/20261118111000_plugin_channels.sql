-- Notification channels run by a plugin (ADR-0103). Only the list of
-- kinds widens; no row changes. `settings` holds the plugin's name and its
-- settings, `secret` its secret settings as a JSON object.
ALTER TABLE notification_channels
    DROP CONSTRAINT notification_channels_kind_check,
    ADD CONSTRAINT notification_channels_kind_check
        CHECK (kind IN ('email', 'webhook', 'slack', 'teams', 'discord', 'plugin'));
