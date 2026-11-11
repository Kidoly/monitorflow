#!/usr/bin/env bash
# Sourced by the deployment tests: what PostgreSQL must say of the role the
# server runs on (ADR-0109). Each query answers t.

# The role is not a superuser, owns nothing, and may add to and read the
# audit log and the remediation history, not change them.
# shellcheck disable=SC2034 # used by the scripts that source this file
APP_ROLE_RESTRICTED="SELECT NOT r.rolsuper AND NOT r.rolcreatedb AND NOT r.rolcreaterole
        AND NOT EXISTS (SELECT 1 FROM pg_class c WHERE c.relowner = r.oid)
        AND NOT has_schema_privilege(r.oid, 'public', 'CREATE')
        AND has_table_privilege(r.oid, 'audit_log', 'INSERT')
        AND has_table_privilege(r.oid, 'audit_log', 'SELECT')
        AND NOT has_table_privilege(r.oid, 'audit_log', 'UPDATE, DELETE, TRUNCATE')
        AND NOT has_table_privilege(r.oid, 'remediation_incident_events', 'UPDATE, DELETE, TRUNCATE')
        AND has_table_privilege(r.oid, 'remediation_incidents', 'UPDATE')
        AND NOT has_table_privilege(r.oid, 'remediation_incidents', 'DELETE, TRUNCATE')
    FROM pg_roles r WHERE r.rolname = 'monitorflow_app'"

# Every connection from the network is the server's role: nothing runs on
# the role that owns the schema. The query's own session is local.
# shellcheck disable=SC2034
SERVER_RUNS_AS_APP="SELECT count(*) > 0 AND bool_and(usename = 'monitorflow_app')
    FROM pg_stat_activity
    WHERE backend_type = 'client backend' AND client_addr IS NOT NULL"
