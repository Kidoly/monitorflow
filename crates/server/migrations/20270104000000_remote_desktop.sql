-- Remote desktops (ADR-0190): the host's own RDP service, tunnelled
-- through the agent's connection, with the sessions, audit and idle time
-- of terminals. The host allows them apart (`remote.desktop` in
-- agent.toml), which no row here can change. Nothing of the screen is
-- recorded.
ALTER TABLE remote_sessions DROP CONSTRAINT remote_sessions_kind_check;
ALTER TABLE remote_sessions
    ADD CONSTRAINT remote_sessions_kind_check CHECK (kind IN ('terminal', 'desktop'));
ALTER TABLE remote_sessions
    ADD CONSTRAINT remote_sessions_desktop_unrecorded CHECK (kind <> 'desktop' OR NOT recorded);
COMMENT ON TABLE remote_sessions IS
    'Remote terminals and desktops: who opened them on which server, when and for how long. See ADR-0190.';
