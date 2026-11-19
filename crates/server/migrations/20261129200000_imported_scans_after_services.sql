-- Imported network scans (ADR-0154) and business services (ADR-0155) were
-- added to what an import records by two migrations made side by side;
-- the later one left network scans out. Every kind, once more.
ALTER TABLE import_created DROP CONSTRAINT import_created_object_check;
ALTER TABLE import_created ADD CONSTRAINT import_created_object_check
    CHECK (object IN ('server', 'check', 'threshold', 'silence', 'window', 'network_scan',
                      'channel', 'dependency', 'service'));
