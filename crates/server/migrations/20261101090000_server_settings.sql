-- Server-wide settings administrators edit in the web interface. One row;
-- NULL leaves a setting to its default. The server's configuration, when
-- it sets the same thing, wins.
CREATE TABLE server_settings (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    -- Default source of the install commands on the tokens page (ADR-0054).
    agent_install_source text
        CHECK (agent_install_source IN ('github', 'central', 'custom')),
    -- https base URL of the `custom` source, without a trailing slash.
    agent_install_custom_url text
        CHECK (agent_install_custom_url LIKE 'https://_%')
);
COMMENT ON TABLE server_settings IS
    'Server-wide settings from the web interface, one row; configuration overrides them. See ADR-0054.';
