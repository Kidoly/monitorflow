-- What someone said they are doing when they acknowledged an incident
-- (ADR-0130). Null for an acknowledgement from a notification link, or
-- from a script that sent none.
ALTER TABLE incident_acknowledgements
    ADD COLUMN note text CHECK (note IS NULL OR char_length(note) BETWEEN 1 AND 500);
