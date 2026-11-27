-- The nodes, namespaces, workloads and failed jobs of a Kubernetes
-- cluster join the inventory of ADR-0156 (ADR-0183). A node is a
-- machine, so it is matched to a server as a virtual machine is.

ALTER TABLE virtual_objects DROP CONSTRAINT virtual_objects_kind_check;
ALTER TABLE virtual_objects ADD CONSTRAINT virtual_objects_kind_check
    CHECK (kind IN ('vm', 'host', 'cluster', 'datastore', 'node', 'namespace', 'workload', 'job'));

ALTER TABLE virtual_objects DROP CONSTRAINT virtual_objects_check;
ALTER TABLE virtual_objects ADD CONSTRAINT virtual_objects_check
    CHECK (kind IN ('vm', 'node') OR (server_id IS NULL AND NOT link_fixed));

COMMENT ON TABLE virtual_objects IS
    'Latest view of each host, cluster, virtual machine and datastore of a source, and of each node, namespace, workload and failed job of a Kubernetes cluster; see ADR-0156 and ADR-0183.';
