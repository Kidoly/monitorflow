-- The language the AI writes its diagnosis in (ADR-0181). NULL follows
-- the language most people chose for the web interface, else English.
ALTER TABLE server_settings
    ADD COLUMN ai_language text CHECK (ai_language IN ('en', 'fr'));
COMMENT ON COLUMN server_settings.ai_language IS
    'Language of AI diagnoses; NULL follows the users'' preferences. See ADR-0181.';
