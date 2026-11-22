-- When each server last started accepting AI commands (ADR-0165): the
-- remediation loop runs an alert's remediation again once, after one that
-- stopped because the server refused them, when this moves past its end.
--
-- Kept by a trigger so every path that stores the agent's report
-- (ADR-0076) moves it: set when `shell` becomes accepted, cleared by a
-- reported refusal, and left alone while the report is unknown (an older
-- relay drops it), so a relay going back and forth starts nothing.
ALTER TABLE servers ADD COLUMN shell_accepted_at timestamptz;
COMMENT ON COLUMN servers.shell_accepted_at IS
    'When command_kinds.shell last became accepted; NULL after a reported refusal. See ADR-0165.';

CREATE FUNCTION servers_shell_accepted_at() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    shell text := NEW.command_kinds->>'shell';
BEGIN
    IF shell = 'accepted' THEN
        IF TG_OP = 'INSERT' OR OLD.shell_accepted_at IS NULL THEN
            NEW.shell_accepted_at := now();
        ELSE
            NEW.shell_accepted_at := OLD.shell_accepted_at;
        END IF;
    ELSIF shell IN ('off', 'unsupported') THEN
        NEW.shell_accepted_at := NULL;
    ELSIF TG_OP = 'UPDATE' THEN
        NEW.shell_accepted_at := OLD.shell_accepted_at;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER servers_shell_accepted_at
    BEFORE INSERT OR UPDATE OF command_kinds, shell_accepted_at ON servers
    FOR EACH ROW EXECUTE FUNCTION servers_shell_accepted_at();

-- Servers accepting them now count as accepting from now on: a
-- remediation that stopped on a refusal before this version, on an alert
-- still open, gets its one new attempt.
UPDATE servers SET shell_accepted_at = now() WHERE command_kinds->>'shell' = 'accepted';
