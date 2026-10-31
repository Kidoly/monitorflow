-- AI set up in the web interface (ADR-0064), in the one row of
-- server_settings. The key is written by the API and never returned, like
-- notification secrets (ADR-0060); `[ai] enabled = true` in the server's
-- configuration wins over all three.
ALTER TABLE server_settings
    ADD COLUMN ai_enabled boolean NOT NULL DEFAULT false,
    ADD COLUMN ai_api_key text,
    ADD COLUMN ai_model text,
    ADD CONSTRAINT server_settings_ai_needs_a_key
        CHECK (NOT ai_enabled OR ai_api_key IS NOT NULL);
COMMENT ON COLUMN server_settings.ai_api_key IS
    'AI provider API key; write-only through the API, see ADR-0064.';
