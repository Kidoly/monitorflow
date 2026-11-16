-- Third-party apps (ADR-0138): what winget, Flatpak and Snap report,
-- kept apart from the OS packages of server_packages. Each package report
-- replaces both tables of its server; one without manager statuses (an
-- older agent, or an older relay on the way) empties them.
CREATE TABLE server_package_managers (
    server_id uuid NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    manager   text NOT NULL CHECK (manager IN ('winget', 'flatpak', 'snap')),
    state     text NOT NULL CHECK (state IN ('available', 'absent', 'unusable')),
    version   text CHECK (version <> '' AND octet_length(version) <= 128),
    error     text CHECK (error <> '' AND octet_length(error) <= 1024),
    PRIMARY KEY (server_id, manager)
);
COMMENT ON TABLE server_package_managers IS
    'Third-party package managers per server from the latest report; see ADR-0138.';

CREATE TABLE server_third_party_packages (
    server_id         uuid NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    manager           text NOT NULL CHECK (manager IN ('winget', 'flatpak', 'snap')),
    package_id        text NOT NULL CHECK (package_id <> '' AND octet_length(package_id) <= 512),
    scope             text NOT NULL CHECK (scope IN ('machine', 'user')),
    name              text CHECK (name <> '' AND octet_length(name) <= 512),
    version           text NOT NULL CHECK (octet_length(version) <= 256),
    -- The version an update would install; NULL when none is pending.
    available_version text CHECK (available_version <> '' AND octet_length(available_version) <= 256),
    origin            text CHECK (origin <> '' AND octet_length(origin) <= 256),
    PRIMARY KEY (server_id, manager, package_id, scope)
);
COMMENT ON TABLE server_third_party_packages IS
    'Apps third-party package managers installed, per server, from the latest report; see ADR-0138.';
CREATE INDEX server_third_party_packages_pending_idx
    ON server_third_party_packages (server_id, manager, package_id, scope)
    WHERE available_version IS NOT NULL;
