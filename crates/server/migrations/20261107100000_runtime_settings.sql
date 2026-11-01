-- Server settings an administrator changes in the web interface
-- (ADR-0069), in the one row of server_settings. NULL follows the default;
-- what the server's configuration sets wins over all three.
ALTER TABLE server_settings
    ADD COLUMN session_lifetime integer CHECK (session_lifetime > 0),
    ADD COLUMN session_idle_timeout integer CHECK (session_idle_timeout > 0),
    ADD COLUMN mirror_public_url text;
COMMENT ON COLUMN server_settings.session_lifetime IS
    'Seconds a session lasts after login; see ADR-0069.';
COMMENT ON COLUMN server_settings.session_idle_timeout IS
    'Seconds without a request after which a session ends; see ADR-0069.';
COMMENT ON COLUMN server_settings.mirror_public_url IS
    'Base URL agents reach the mirror listener at; see ADR-0069.';
