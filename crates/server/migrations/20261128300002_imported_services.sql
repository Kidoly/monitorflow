-- An import may create business services (ADR-0155), which its undo
-- removes like the rest of what it made. 'dependency' is ADR-0152's,
-- whose migration rewrites this constraint too: both keep both, in
-- whichever order they run.
ALTER TABLE import_created DROP CONSTRAINT import_created_object_check;
ALTER TABLE import_created ADD CONSTRAINT import_created_object_check
    CHECK (object IN ('server', 'check', 'threshold', 'silence', 'window', 'channel',
                      'dependency', 'service'));
