-- The address hosts reach this server at, in the install commands of the
-- tokens page: a DNS name or an IP address, without a port (the commands
-- use the published ports). NULL follows the first of `server_names`.
ALTER TABLE server_settings
    ADD COLUMN public_address text
        CHECK (public_address ~ '^[A-Za-z0-9.:-]{1,253}$');
COMMENT ON COLUMN server_settings.public_address IS
    'Address in the install commands; NULL is the first configured server name.';
