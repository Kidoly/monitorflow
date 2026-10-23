"use client";

import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { PageHeader, StaleBanner } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";
import { type Customer, type Me, type RoleGrant, type Site, api, describeError } from "@/lib/api";
import { usePolledData } from "@/lib/usePolledData";

import { AdminLayout } from "./AdminLayout";
import styles from "./AdminPages.module.css";

/**
 * `/admin/users/`. The REST API has no users routes yet: accounts and
 * grants are managed with the server CLI (`crates/server/src/api/users.rs`),
 * and `/api/auth/me` is the only account it returns. So the page lists the
 * signed-in account and its grants, without the design's actions.
 */
export function UsersAdminPage() {
  return <AdminLayout>{(me) => <UsersAdmin me={me} />}</AdminLayout>;
}

interface Names {
  customers: Customer[];
  sites: Site[];
}

async function load(signal: AbortSignal): Promise<Names> {
  const [{ customers }, { sites }] = await Promise.all([api.customers(signal), api.sites(signal)]);
  return { customers, sites };
}

/** "administrator · global", "operator · Acme Hosting", "viewer · Acme › lyon-office". */
export function describeGrant(grant: RoleGrant, names: Names | undefined): string {
  if (grant.site_id) {
    const site = names?.sites.find((s) => s.id === grant.site_id);
    return `${grant.role} · ${site ? `${site.customer_name} › ${site.name}` : "a site"}`;
  }
  if (grant.customer_id) {
    const customer = names?.customers.find((c) => c.id === grant.customer_id);
    return `${grant.role} · ${customer?.name ?? "a customer"}`;
  }
  return `${grant.role} · global`;
}

export function UsersAdmin({ me }: { me: Me }) {
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(load);

  const columns: Column<Me>[] = [
    { key: "user", header: "User", cell: (user) => <PrimaryCell title={user.username} sub="Signed in now" /> },
    { key: "state", header: "State", cell: () => <Label tone="ok">Active</Label> },
    {
      key: "grants",
      header: "Grants",
      className: styles.wrap,
      cell: (user) => user.roles.map((grant) => describeGrant(grant, data)).join("\u00a0 ·\u00a0 ") || "No role",
    },
  ];

  return (
    <div className={styles.page}>
      <PageHeader title="Users and roles" meta="Roles are granted globally, per customer or per site" />
      {stale && updatedAt && (
        <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />
      )}
      <DataTable dense label="Users" columns={columns} rows={[me]} rowKey={(user) => user.id} />
      <p className={styles.footnote}>
        The web interface shows only your own account for now. Create users and grant roles on the server with{" "}
        <code>monitorflow-server user add</code> and <code>monitorflow-server user grant</code>.
      </p>
    </div>
  );
}
