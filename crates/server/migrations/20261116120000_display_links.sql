-- Display links (ADR-0099): a link that shows one dashboard, or several in
-- turn, on a wall screen without signing in. It acts with the rights of the
-- person who made it, checked again on every request. Only the SHA-256 of
-- its secret is stored.

CREATE TABLE display_links (
    id               uuid        PRIMARY KEY DEFAULT gen_random_uuid(),
    token_hash       bytea       NOT NULL UNIQUE,
    -- Where the screen is: "Reception TV".
    name             text        NOT NULL CHECK (name <> '' AND char_length(name) <= 80),
    created_by       uuid        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at       timestamptz NOT NULL DEFAULT now(),
    -- NULL: never expires.
    expires_at       timestamptz,
    seconds_per_page integer     NOT NULL CHECK (seconds_per_page BETWEEN 10 AND 3600),
    last_used_at     timestamptz
);
COMMENT ON TABLE display_links IS
    'Links that show dashboards on a screen without signing in. See ADR-0099.';
CREATE INDEX display_links_created_by_idx ON display_links (created_by);

-- The dashboards a link shows, in order. Deleting a dashboard takes it out.
CREATE TABLE display_link_dashboards (
    link_id      uuid     NOT NULL REFERENCES display_links (id) ON DELETE CASCADE,
    position     smallint NOT NULL CHECK (position >= 0),
    dashboard_id uuid     NOT NULL REFERENCES dashboards (id) ON DELETE CASCADE,
    PRIMARY KEY (link_id, position),
    UNIQUE (link_id, dashboard_id)
);
CREATE INDEX display_link_dashboards_dashboard_id_idx ON display_link_dashboards (dashboard_id);

-- What a screen holds after opening a link: a short session in a cookie,
-- so the link's secret crosses the network once an hour, not on every
-- refresh. Turning the link off ends them.
CREATE TABLE display_sessions (
    token_hash bytea       PRIMARY KEY,
    link_id    uuid        NOT NULL REFERENCES display_links (id) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL
);
CREATE INDEX display_sessions_link_id_idx ON display_sessions (link_id);
CREATE INDEX display_sessions_expires_at_idx ON display_sessions (expires_at);
