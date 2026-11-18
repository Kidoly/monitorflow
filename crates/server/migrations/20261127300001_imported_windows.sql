-- An import may create maintenance windows (ADR-0151), which its undo
-- removes like the rest of what it made.
ALTER TABLE import_created DROP CONSTRAINT import_created_object_check;
ALTER TABLE import_created ADD CONSTRAINT import_created_object_check
    CHECK (object IN ('server', 'check', 'threshold', 'silence', 'window', 'channel'));
