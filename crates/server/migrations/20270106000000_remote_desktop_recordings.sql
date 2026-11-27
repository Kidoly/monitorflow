-- Desktop recordings (ADR-0192): a recorded desktop's screen, decoded by
-- the central server from the RDP it proxies, kept as PNG pictures. The
-- `record` setting now covers desktops as it covers terminals.
ALTER TABLE remote_sessions DROP CONSTRAINT remote_sessions_desktop_unrecorded;

-- Pictures of the screen, in order: at most one a second, of the part
-- that changed, and of the whole screen at the start, when its size
-- changes and once a minute while it changes, so a replay can start at
-- any whole picture.
CREATE TABLE remote_desktop_frames (
    session_id    uuid    NOT NULL REFERENCES remote_sessions (id) ON DELETE CASCADE,
    seq           integer NOT NULL,
    -- Milliseconds since the session opened.
    at_ms         bigint  NOT NULL CHECK (at_ms >= 0),
    -- The part of the screen the picture covers.
    x             integer NOT NULL CHECK (x >= 0),
    y             integer NOT NULL CHECK (y >= 0),
    width         integer NOT NULL CHECK (width > 0),
    height        integer NOT NULL CHECK (height > 0),
    -- The size of the whole screen when it was taken.
    screen_width  integer NOT NULL CHECK (screen_width > 0 AND x + width <= screen_width),
    screen_height integer NOT NULL CHECK (screen_height > 0 AND y + height <= screen_height),
    image         bytea   NOT NULL,
    PRIMARY KEY (session_id, seq)
);
COMMENT ON TABLE remote_desktop_frames IS
    'The screen of recorded remote desktops, as PNG pictures, for replay. See ADR-0192.';
