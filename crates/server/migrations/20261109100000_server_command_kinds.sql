-- What each server's agent does with each kind of command (ADR-0076), from
-- its latest Hello: {"shell": "off", "apply_updates": "accepted", ...}. NULL
-- is not reported, which says nothing either way, unlike any value here.
ALTER TABLE servers ADD COLUMN command_kinds JSONB;
