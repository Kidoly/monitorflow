"use client";

import { type FormEvent, type RefObject, useId, useRef, useState } from "react";

import { Button, fieldClass } from "@/components/ui/Button";
import { type PlainKey, t } from "@/i18n";
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
import { placeLabel } from "@/lib/customers";
import { administersEverything, multiCustomer, useMultiCustomer } from "@/lib/me";
import { usePolledData } from "@/lib/usePolledData";

import { AdminLayout } from "./AdminLayout";
import styles from "./AdminPages.module.css";

/**
 * `/settings/users/` (ADR-0059). Administrators of everything invite users by
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

/**
 * "administrator · everything", "operator · Acme Hosting", "viewer · Acme ›
 * lyon-office"; without customers (ADR-0104), "operator · all sites" and
 * "viewer · lyon-office".
 */
export function describeGrant(grant: RoleGrant, names: Names | undefined, multiCustomer = true): string {
  return `${t(`common.role.${grant.role}`)} · ${describeScope(grant, names, multiCustomer)}`;
}

function describeScope(
  grant: { customer_id?: string | null; site_id?: string | null },
  names: Names | undefined,
  multiCustomer: boolean,
) {
  if (grant.site_id) {
    const site = names?.sites.find((s) => s.id === grant.site_id);
    return site ? placeLabel(site.customer_name, site.name, multiCustomer) : t("users.aSite");
  }
  if (grant.customer_id) {
    if (!multiCustomer) return t("users.allSites");
    return names?.customers.find((c) => c.id === grant.customer_id)?.name ?? t("users.aCustomer");
  }
  return t("users.everything");
}

const roleReach = (multiCustomer: boolean) =>
  multiCustomer ? t("users.reachCustomers") : t("users.reach");

const ROLES: { role: Role; name: PlainKey; hint: PlainKey }[] = [
  { role: "viewer", name: "users.role.viewer", hint: "users.hint.viewer" },
  { role: "operator", name: "users.role.operator", hint: "users.hint.operator" },
  { role: "approver", name: "users.role.approver", hint: "users.hint.approver" },
  { role: "administrator", name: "users.role.administrator", hint: "users.hint.administrator" },
];

const roleHint = (role: Role) => t(ROLES.find((r) => r.role === role)!.hint);

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
  const multiCustomer = useMultiCustomer();
  const siteOption = (site: Site) => (
    <option key={site.id} value={`s:${site.id}`}>
      {t("users.siteOption", { name: site.name })}
    </option>
  );
  return (
    <>
      <label className={styles.field}>
        {t("users.roleField")}
        <select className={fieldClass} value={role} onChange={(e) => onRole(e.target.value as Role)}>
          {ROLES.map((r) => (
            <option key={r.role} value={r.role}>
              {t(r.name)}
            </option>
          ))}
        </select>
      </label>
      <label className={styles.field}>
        {t("users.onField")}
        <select className={fieldClass} value={scope} onChange={(e) => onScope(e.target.value)}>
          <option value="">{t("users.everythingOption")}</option>
          {/* A single company has everything and its sites (ADR-0104). */}
          {!multiCustomer && names.sites.map(siteOption)}
          {multiCustomer &&
            names.customers.map((customer) => (
              <optgroup key={customer.id} label={customer.name}>
                <option value={`c:${customer.id}`}>{t("users.allOf", { name: customer.name })}</option>
                {names.sites.filter((site) => site.customer_id === customer.id).map(siteOption)}
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
        label={invite ? t("users.invitationLink", { username }) : t("users.passwordLink", { username })}
        value={passwordLinkUrl(link)}
        copyName={t("users.copyLink")}
      />
      <span className={styles.hint}>
        {invite ? t("users.sendInvitation", { username }) : t("users.sendPasswordLink", { username })}
      </span>
    </div>
  );
}

function InviteForm({ names, onInvited, inputRef }: { names: Names; onInvited: () => void; inputRef: RefObject<HTMLInputElement | null> }) {
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
          : t("users.inviteFailed", { error: describeError(e) }),
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className={styles.panel} aria-labelledby={titleId}>
      <h2 id={titleId} className={styles.sectionTitle}>
        {t("users.invite")}
      </h2>
      {invited ? (
        <>
          <LinkPanel username={invited.username} link={invited.link} invite />
          <div className={styles.buttons}>
            <Button variant="k" onClick={() => setInvited(null)}>
              {t("users.done")}
            </Button>
          </div>
        </>
      ) : (
        <form className={styles.formGrid} onSubmit={submit} aria-label={t("users.invite")}>
          <label className={styles.field}>
            {t("users.username")}
            <input
              ref={inputRef}
              className={fieldClass}
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              placeholder={t("users.usernamePlaceholder")}
              autoCapitalize="none"
              spellCheck={false}
              required
            />
          </label>
          <GrantFields names={names} role={role} scope={scope} onRole={setRole} onScope={setScope} />
          <Button variant="k" type="submit" disabled={busy || username.trim() === ""}>
            {busy ? t("users.creating") : t("users.createLink")}
          </Button>
          <span className={error ? styles.hintError : styles.hint} role={error ? "alert" : undefined}>
            {error ?? roleHint(role)}
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
    <form className={styles.formGrid} onSubmit={submit} aria-label={t("users.addRoleTo", { username: user.username })}>
      <GrantFields names={names} role={role} scope={scope} onRole={setRole} onScope={setScope} />
      <Button variant="k" type="submit" disabled={busy}>
        {t("users.addRole")}
      </Button>
      <span className={error ? styles.hintError : styles.hint} role={error ? "alert" : undefined}>
        {error ?? roleHint(role)}
      </span>
    </form>
  );
}

/** What is open under a row. */
type Open = { id: string; kind: "role" } | { id: string; kind: "link"; link: PasswordLink };

export function UsersAdmin({ me }: { me: Me }) {
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(load);
  const [open, setOpen] = useState<Open | null>(null);
  const invite = useRef<HTMLInputElement>(null);
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
      header: t("users.column.user"),
      cell: (user) => (
        <PrimaryCell
          title={user.username}
          sub={
            user.id === me.id
              ? t("users.you")
              : user.state === "invited"
                ? user.link_expires_at
                  ? t("users.notOpened")
                  : t("users.expired")
                : undefined
          }
        />
      ),
    },
    {
      key: "state",
      header: t("users.column.state"),
      cell: (user) =>
        user.state === "active" ? (
          <Label tone="ok">{t("users.active")}</Label>
        ) : user.state === "invited" ? (
          <Label tone="warn">{t("users.invited")}</Label>
        ) : (
          <Label tone="neutral">{t("users.disabled")}</Label>
        ),
    },
    {
      key: "roles",
      header: t("users.column.roles"),
      className: styles.wrap,
      cell: (user) =>
        user.roles.length === 0 ? (
          <span className={styles.dim}>{t("users.noRoleSeesNothing")}</span>
        ) : (
          user.roles.map((grant) => {
            const text = describeGrant(grant, data, multiCustomer(me));
            return (
              <span key={text} className={styles.grant}>
                {text}
                <button
                  type="button"
                  className={styles.grantRemove}
                  aria-label={t("users.revoke", { grant: text, username: user.username })}
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
      <PageHeader title={t("settings.users.label")} meta={roleReach(multiCustomer(me))} />
      {stale && updatedAt && (
        <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />
      )}
      {!data && error ? <ErrorState what={t("users.what")} error={describeError(error)} onRetry={refresh} /> : null}
      {!data && !error && <LoadingBlock label={t("users.loading")} />}
      {data && (
        <>
          <InviteForm names={data} onInvited={refresh} inputRef={invite} />
          <DataTable
            dense
            label={t("users.tableLabel")}
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
                  {t("users.addRole")}
                </Button>
                {user.state !== "disabled" && user.id !== me.id && (
                  <Button size="small" onClick={() => void issueLink(user)}>
                    {user.state === "invited" ? t("users.newInvitation") : t("users.resetPassword")}
                  </Button>
                )}
                {user.id !== me.id &&
                  (user.state === "disabled" ? (
                    <Button size="small" onClick={() => void act(user, () => api.setUserDisabled(user.id, false))}>
                      {t("users.enable")}
                    </Button>
                  ) : (
                    <Button size="small" onClick={() => void act(user, () => api.setUserDisabled(user.id, true))}>
                      {t("users.disable")}
                    </Button>
                  ))}
                {user.id !== me.id && (
                  <InlineConfirm
                    layout="row"
                    trigger={t("users.remove")}
                    triggerName={t("users.removeName", { username: user.username })}
                    title={t("users.removeTitle", { username: user.username })}
                    consequence={t("users.removeConsequence")}
                    confirmLabel={t("users.removeConfirm")}
                    pendingLabel={t("users.removing")}
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
            <EmptyState
              title={t("users.onlyYou")}
              actions={
                <Button variant="k" onClick={() => invite.current?.focus()}>
                  {t("users.inviteColleague")}
                </Button>
              }
            >
              {t("users.onlyYouText")}
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
    {
      key: "user",
      header: t("users.column.user"),
      cell: (user) => <PrimaryCell title={user.username} sub={t("users.signedInNow")} />,
    },
    { key: "state", header: t("users.column.state"), cell: () => <Label tone="ok">{t("users.active")}</Label> },
    {
      key: "grants",
      header: t("users.column.grants"),
      className: styles.wrap,
      cell: (user) =>
        user.roles.map((grant) => describeGrant(grant, data, multiCustomer(me))).join("  ·  ") || t("users.noRole"),
    },
  ];
  return (
    <div className={styles.page}>
      <PageHeader title={t("settings.users.label")} meta={roleReach(multiCustomer(me))} />
      <DataTable dense label={t("users.tableLabel")} columns={columns} rows={[me]} rowKey={(user) => user.id} />
      <Banner tone="info">{t("users.onlyGlobal")}</Banner>
    </div>
  );
}
