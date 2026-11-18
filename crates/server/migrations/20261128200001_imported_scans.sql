-- An import may create network scans (ADR-0154), which its undo removes
-- like the rest of what it made.
ALTER TABLE import_created DROP CONSTRAINT import_created_object_check;
ALTER TABLE import_created ADD CONSTRAINT import_created_object_check
    CHECK (object IN ('server', 'check', 'threshold', 'silence', 'window', 'network_scan',
                      'channel'));
