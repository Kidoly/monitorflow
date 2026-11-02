-- Slack, Teams and Discord channels (ADR-0077). Only the list of kinds
-- grows: no existing row changes.
--
-- The webhook URL of these kinds is a credential, so it lives in `secret`,
-- which the API never returns; `settings` holds {"name", "host"}.
ALTER TABLE notification_channels
    DROP CONSTRAINT notification_channels_kind_check,
    ADD CONSTRAINT notification_channels_kind_check
        CHECK (kind IN ('email', 'webhook', 'slack', 'teams', 'discord'));

COMMENT ON TABLE notification_channels IS
    'Email, webhook and chat destinations of notifications; secret never leaves the server. See ADR-0060 and ADR-0077.';
