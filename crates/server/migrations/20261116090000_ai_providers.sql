-- Any AI provider in the web interface (ADR-0096): Anthropic, OpenAI, or
-- an OpenAI-compatible server at an address, which may need no key.
ALTER TABLE server_settings
    ADD COLUMN ai_provider text NOT NULL DEFAULT 'anthropic'
        CONSTRAINT server_settings_ai_provider_known
        CHECK (ai_provider IN ('anthropic', 'openai', 'openai_compatible')),
    ADD COLUMN ai_base_url text,
    DROP CONSTRAINT server_settings_ai_needs_a_key,
    ADD CONSTRAINT server_settings_ai_needs_a_key
        CHECK (NOT ai_enabled OR ai_api_key IS NOT NULL OR ai_provider = 'openai_compatible'),
    ADD CONSTRAINT server_settings_ai_needs_an_address
        CHECK (NOT ai_enabled OR ai_provider <> 'openai_compatible' OR ai_base_url IS NOT NULL);
COMMENT ON COLUMN server_settings.ai_base_url IS
    'API root of an OpenAI-compatible server, like http://ollama.lan:11434/v1; see ADR-0096.';
