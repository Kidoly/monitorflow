"use client";

import { type FormEvent, useId, useState } from "react";

import { Button, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { Banner, EmptyState, ErrorState, LoadingBlock, PageHeader, StaleBanner } from "@/components/ui/PageState";
import { SecretField } from "@/components/ui/SecretField";
import { Label } from "@/components/ui/StatusLabel";
import {
  ApiError,
  type Customer,
  type GrantRequest,
  type Me,
  type PasswordLink,
  type Role,
  type RoleGrant,
  type Site,
  type UserAccount,
  api,
  describeError,
  passwordLinkUrl,
} from "@/lib/api";
import { administersEverything } from "@/lib/me";
import { usePolledData } from "@/lib/usePolledData";

import { AdminLayout } from "./AdminLayout";
import styles from "./AdminPages.module.css";

/**
 * `/admin/users/` (ADR-0058). Administrators of everything invite users by
 * one-time link, grant and revoke roles, issue password links, and disable
 * or remove accounts. Other administrators see their own account.
 */
export function UsersAdminPage() {
  return (
    <AdminLayout>{(me) => (administersEverything(me) ? <UsersAdmin me={me} /> : <OwnAccount me={me} />)}</AdminLayout>
  );
}

interface Names {
  customers: Customer[];
  sites: Site[];
}

async function loadNames(signal: AbortSignal): Promise<Names> {
  const [{ customers }, { sites }] = await Promise.all([api.customers(signal), api.sites(signal)]);
  return { customers, sites };
}

interface Data extends Names {
  users: UserAccount[];
}

async function load(signal: AbortSignal): Promise<Data> {
  const [names, { users }] = await Promise.all([loadNames(signal), api.users(signal)]);
  return { ...names, users };
}

/** "administrator · everything", "operator · Acme Hosting", "viewer · Acme › lyon-office". */
export function describeGrant(grant: RoleGrant, names: Names | undefined): string {
  return `${grant.role} · ${describeScope(grant, names)}`;
}

function describeScope(grant: { customer_id?: string | null; site_id?: string | null }, names: Names | undefined) {
  if (grant.site_id) {
    const site = names?.sites.find((s) => s.id === grant.site_id);
    return site ? `${site.customer_name} › ${site.name}` : "a site";
  }
  if (grant.customer_id) {
    return names?.customers.find((c) => c.id === grant.customer_id)?.name ?? "a customer";
  }
  return "everything";
}

const ROLES: { role: Role; hint: string }[] = [
  { role: "viewer", hint: "Sees servers, metrics and incidents." },
  { role: "operator", hint: "Also changes alerts and checks, and runs updates." },
  { role: "approver", hint: "Also approves low-impact AI commands." },
  { role: "administrator", hint: "Also manages sites, install tokens and, on everything, users." },
];

/** Value of the scope select: `""` for everything, `c:<id>` or `s:<id>`. */
function grantOf(role: Role, scope: string): GrantRequest {
  if (scope.startsWith("c:")) return { role, customer_id: scope.slice(2) };
  if (scope.startsWith("s:")) return { role, site_id: scope.slice(2) };
  return { role };
}

function toRequest(grant: RoleGrant): GrantRequest {
  return grantOf(grant.role, grant.site_id ? `s:${grant.site_id}` : grant.customer_id ? `c:${grant.customer_id}` : "");
}

/** Role and scope selects, shared by the invitation and "Add role". */
function GrantFields({
  names,
  role,
  scope,
  onRole,
  onScope,
}: {
  names: Names;
  role: Role;
  scope: string;
  onRole: (role: Role) => void;
  onScope: (scope: string) => void;
}) {
  return (
    <>
      <label className={styles.field}>
        Role
        <select className={fieldClass} value={role} onChange={(e) => onRole(e.target.value as Role)}>
          {ROLES.map((r) => (
            <option key={r.role} value={r.role}>
              {r.role.charAt(0).toUpperCase() + r.role.slice(1)}
            </option>
          ))}
        </select>
      </label>
      <label className={styles.field}>
        On
        <select className={fieldClass} value={scope} onChange={(e) => onScope(e.target.value)}>
          <option value="">Everything</option>
          {names.customers.map((customer) => (
            <optgroup key={customer.id} label={customer.name}>
              <option value={`c:${customer.id}`}>All of {customer.name}</option>
              {names.sites
                .filter((site) => site.customer_id === customer.id)
                .map((site) => (
                  <option key={site.id} value={`s:${site.id}`}>
                    Site {site.name}
                  </option>
                ))}
            </optgroup>
          ))}
        </select>
      </label>
    </>
  );
}

/** A password link just issued, with what to do with it. */
function LinkPanel({ username, link, invite }: { username: string; link: PasswordLink; invite: boolean }) {
  return (
    <div className={styles.stack}>
      <SecretField
        label={invite ? `Invitation link for ${username}` : `Password link for ${username}`}
        value={passwordLinkUrl(link)}
        copyName="Copy link"
      />
      <span className={styles.hint}>
        Send it to {username}. It works once, for 7 days: {username} chooses a password and is signed in.
        {!invite && " Their current password keeps working until then."} It is only shown now.
      </span>
    </div>
  );
}

function InviteForm({ names, onInvited }: { names: Names; onInvited: () => void }) {
  const titleId = useId();
  const [username, setUsername] = useState("");
  const [role, setRole] = useState<Role>("viewer");
  const [scope, setScope] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [invited, setInvited] = useState<{ username: string; link: PasswordLink } | null>(null);

  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const { user, link } = await api.inviteUser(username.trim(), grantOf(role, scope));
      setInvited({ username: user.username, link });
      setUsername("");
      onInvited();
    } catch (e) {
      setError(
        e instanceof ApiError && (e.status === 409 || e.status === 400)
          ? `${e.message.charAt(0).toUpperCase()}${e.message.slice(1)}.`
          : `Creating the invitation failed. ${describeError(e)}`,
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className={styles.panel} aria-labelledby={titleId}>
      <h2 id={titleId} className={styles.sectionTitle}>
        Invite a user
      </h2>
      {invited ? (
        <>
          <LinkPanel username={invited.username} link={invited.link} invite />
          <div className={styles.buttons}>
            <Button variant="k" onClick={() => setInvited(null)}>
              Done
            </Button>
          </div>
        </>
      ) : (
        <form className={styles.formGrid} onSubmit={submit} aria-label="Invite a user">
          <label className={styles.field}>
            Username
            <input
              className={fieldClass}
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              placeholder="e.g. alice"
              autoCapitalize="none"
              spellCheck={false}
              required
            />
          </label>
          <GrantFields names={names} role={role} scope={scope} onRole={setRole} onScope={setScope} />
          <Button variant="k" type="submit" disabled={busy || username.trim() === ""}>
            {busy ? "Creating…" : "Create invitation link"}
          </Button>
          <span className={error ? styles.hintError : styles.hint} role={error ? "alert" : undefined}>
            {error ?? ROLES.find((r) => r.role === role)?.hint}
          </span>
        </form>
      )}
    </section>
  );
}

function AddRole({ user, names, onDone }: { user: UserAccount; names: Names; onDone: () => void }) {
  const [role, setRole] = useState<Role>("viewer");
  const [scope, setScope] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api.grantRole(user.id, grantOf(role, scope));
      onDone();
    } catch (e) {
      setError(describeError(e));
      setBusy(false);
    }
  }

  return (
    <form className={styles.formGrid} onSubmit={submit} aria-label={`Add a role to ${user.username}`}>
      <GrantFields names={names} role={role} scope={scope} onRole={setRole} onScope={setScope} />
      <Button variant="k" type="submit" disabled={busy}>
        Add role
      </Button>
      <span className={error ? styles.hintError : styles.hint} role={error ? "alert" : undefined}>
        {error ?? ROLES.find((r) => r.role === role)?.hint}
      </span>
    </form>
  );
}

/** What is open under a row. */
type Open = { id: string; kind: "role" } | { id: string; kind: "link"; link: PasswordLink };

export function UsersAdmin({ me }: { me: Me }) {
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(load);
  const [open, setOpen] = useState<Open | null>(null);
  const [rowError, setRowError] = useState<{ id: string; text: string } | null>(null);

  async function act(user: UserAccount, action: () => Promise<unknown>) {
    setRowError(null);
    try {
      await action();
      refresh();
    } catch (e) {
      setRowError({
        id: user.id,
        text: e instanceof ApiError && e.status === 409 ? `${e.message.charAt(0).toUpperCase()}${e.message.slice(1)}.` : describeError(e),
      });
    }
  }

  async function issueLink(user: UserAccount) {
    await act(user, async () => setOpen({ id: user.id, kind: "link", link: await api.passwordLink(user.id) }));
  }

  const columns: Column<UserAccount>[] = [
    {
      key: "user",
      header: "User",
      cell: (user) => (
        <PrimaryCell
          title={user.username}
          sub={
            user.id === me.id
              ? "You"
              : user.state === "invited"
                ? user.link_expires_at
                  ? "Has not opened the invitation yet"
                  : "Invitation expired: issue a new link"
                : undefined
          }
        />
      ),
    },
    {
      key: "state",
      header: "State",
      cell: (user) =>
        user.state === "active" ? (
          <Label tone="ok">Active</Label>
        ) : user.state === "invited" ? (
          <Label tone="warn">Invited</Label>
        ) : (
          <Label tone="neutral">Disabled</Label>
        ),
    },
    {
      key: "roles",
      header: "Roles",
      className: styles.wrap,
      cell: (user) =>
        user.roles.length === 0 ? (
          <span className={styles.dim}>No role: sees nothing</span>
        ) : (
          user.roles.map((grant) => {
            const text = describeGrant(grant, data);
            return (
              <span key={text} className={styles.grant}>
                {text}
                <button
                  type="button"
                  className={styles.grantRemove}
                  aria-label={`Revoke ${text} from ${user.username}`}
                  onClick={() => void act(user, () => api.revokeRole(user.id, toRequest(grant)))}
                >
                  ×
                </button>
              </span>
            );
          })
        ),
    },
  ];

  const others = data?.users.filter((user) => user.id !== me.id).length ?? 0;
  return (
    <div className={styles.page}>
      <PageHeader title="Users and roles" meta="A role applies to everything, one customer, or one site" />
      {stale && updatedAt && (
        <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />
      )}
      {!data && error ? <ErrorState what="users" error={describeError(error)} onRetry={refresh} /> : null}
      {!data && !error && <LoadingBlock label="Loading users" />}
      {data && (
        <>
          <InviteForm names={data} onInvited={refresh} />
          <DataTable
            dense
            label="Users"
            columns={columns}
            rows={data.users}
            rowKey={(user) => user.id}
            rowMuted={(user) => user.state === "disabled"}
            rowDetail={(user) => {
              const error = rowError?.id === user.id && (
                <span className={styles.hintError} role="alert">
                  {rowError.text}
                </span>
              );
              if (open?.id !== user.id) return error || null;
              return (
                <>
                  {open.kind === "role" ? (
                    <AddRole
                      user={user}
                      names={data}
                      onDone={() => {
                        setOpen(null);
                        refresh();
                      }}
                    />
                  ) : (
                    <LinkPanel username={user.username} link={open.link} invite={user.state === "invited"} />
                  )}
                  {error}
                </>
              );
            }}
            rowActions={(user) => (
              <span className={styles.buttons}>
                <Button
                  size="small"
                  aria-expanded={open?.id === user.id && open.kind === "role"}
                  onClick={() => setOpen(open?.id === user.id && open.kind === "role" ? null : { id: user.id, kind: "role" })}
                >
                  Add role
                </Button>
                {user.state !== "disabled" && user.id !== me.id && (
                  <Button size="small" onClick={() => void issueLink(user)}>
                    {user.state === "invited" ? "New invitation link" : "Reset password"}
                  </Button>
                )}
                {user.id !== me.id &&
                  (user.state === "disabled" ? (
                    <Button size="small" onClick={() => void act(user, () => api.setUserDisabled(user.id, false))}>
                      Enable
                    </Button>
                  ) : (
                    <Button size="small" onClick={() => void act(user, () => api.setUserDisabled(user.id, true))}>
                      Disable
                    </Button>
                  ))}
                {user.id !== me.id && (
                  <InlineConfirm
                    layout="row"
                    trigger="Remove"
                    triggerName={`Remove ${user.username}`}
                    title={`Remove ${user.username}?`}
                    consequence="The account goes away. What they did stays in the history."
                    confirmLabel="Remove user"
                    pendingLabel="Removing…"
                    describeError={(e) => (e instanceof ApiError && e.status === 409 ? e.message : describeError(e))}
                    onConfirm={async () => {
                      await api.removeUser(user.id);
                      refresh();
                    }}
                  />
                )}
              </span>
            )}
          />
          {others === 0 && (
            <EmptyState title="Only you so far">
              Invite a colleague above. They get a link to choose their password, with the role you pick.
            </EmptyState>
          )}
        </>
      )}
    </div>
  );
}

/** What an administrator of a customer or site sees: their own account. */
function OwnAccount({ me }: { me: Me }) {
  const { data } = usePolledData(loadNames);
  const columns: Column<Me>[] = [
    { key: "user", header: "User", cell: (user) => <PrimaryCell title={user.username} sub="Signed in now" /> },
    { key: "state", header: "State", cell: () => <Label tone="ok">Active</Label> },
    {
      key: "grants",
      header: "Grants",
      className: styles.wrap,
      cell: (user) => user.roles.map((grant) => describeGrant(grant, data)).join("  ·  ") || "No role",
    },
  ];
  return (
    <div className={styles.page}>
      <PageHeader title="Users and roles" meta="A role applies to everything, one customer, or one site" />
      <DataTable dense label="Users" columns={columns} rows={[me]} rowKey={(user) => user.id} />
      <Banner tone="info">Only an administrator of everything invites users and changes roles. Ask one to add a colleague.</Banner>
    </div>
  );
}
