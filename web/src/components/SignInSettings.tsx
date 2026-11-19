"use client";

import { type FormEvent, useCallback, useId, useState } from "react";

import { t } from "@/i18n";
import {
  ApiError,
  type GroupMapping,
  type IdentityProvider,
  type IdentityProviderKind,
  type IdentityProviderList,
  type IdentityProviderSettings,
  type IdentityProviderTestResult,
  type LdapSettings,
  type OidcSettings,
  type Role,
  api,
  describeError,
} from "@/lib/api";
import { administersEverything, useMe, useMultiCustomer } from "@/lib/me";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./AdminPages.module.css";
import { describeGrant } from "./UsersAdmin";
import { Button, fieldClass } from "./ui/Button";
import { cx } from "./ui/cx";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner, ErrorState, LoadingBlock, NoPermission, PageHeader } from "./ui/PageState";
import { CopyButton } from "./ui/SecretField";
import { Label } from "./ui/StatusLabel";

/** `/settings/sign-in/`: global administrators only, like the API (ADR-0145). */
export function SignInSettingsPage() {
  const me = useMe();
  if (!me) return null;
  if (!administersEverything(me)) {
    return <NoPermission title={t("signIn.needAdmin")}>{t("signIn.needAdminBody")}</NoPermission>;
  }
  return <SignIn />;
}

const ROLES: Role[] = ["viewer", "operator", "approver", "administrator"];

const OIDC_DEFAULTS: OidcSettings = {
  kind: "oidc",
  issuer: "",
  client_id: "",
  scopes: "openid profile email groups",
  groups_claim: "groups",
  ca_pem: null,
};

/** Filters of the two usual kinds of directory. */
const PRESETS = {
  openldap: {
    user_filter: "(|(uid={username})(sAMAccountName={username}))",
    group_filter: "(|(member={dn})(uniqueMember={dn})(memberUid={username}))",
    group_name_attribute: "cn",
  },
  ad: {
    user_filter: "(&(objectClass=user)(sAMAccountName={username}))",
    group_filter: "(&(objectClass=group)(member={dn}))",
    group_name_attribute: "cn",
  },
} as const;

const LDAP_DEFAULTS: LdapSettings = {
  kind: "ldap",
  url: "",
  starttls: false,
  ca_pem: null,
  bind_dn: "",
  user_base_dn: "",
  group_base_dn: null,
  nested_groups: true,
  ...PRESETS.openldap,
};

/** What is open below the list. */
type Editing = { kind: IdentityProviderKind; provider: IdentityProvider | null } | null;

function SignIn() {
  const fetcher = useCallback((signal: AbortSignal) => api.identityProviders(signal), []);
  const { data, error, refresh } = usePolledData(fetcher);
  const [editing, setEditing] = useState<Editing>(null);

  const header = <PageHeader title={t("signIn.title")} meta={t("signIn.meta")} />;
  if (!data) {
    return (
      <div className={styles.page}>
        {header}
        {error ? <ErrorState what={t("signIn.what")} error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </div>
    );
  }
  // The provider being edited, as the last read has it.
  const current = editing?.provider ? (data.providers.find((p) => p.id === editing.provider!.id) ?? null) : null;

  return (
    <div className={styles.page}>
      {header}
      <Banner tone="info">{t("signIn.localAdmins", { count: data.local_administrators })}</Banner>
      {data.providers.length === 0 && !editing && <p className={styles.sectionNote}>{t("signIn.none")}</p>}
      {data.providers.map((provider) => (
        <ProviderRow
          key={provider.id}
          provider={provider}
          open={editing?.provider?.id === provider.id}
          onEdit={() => setEditing(editing?.provider?.id === provider.id ? null : { kind: provider.kind, provider })}
          onRemoved={() => {
            setEditing(null);
            refresh();
          }}
        />
      ))}
      {editing && (
        <ProviderForm
          key={editing.provider?.id ?? `new-${editing.kind}`}
          kind={editing.kind}
          provider={current}
          list={data}
          onSaved={(saved) => {
            setEditing({ kind: saved.kind, provider: saved });
            refresh();
          }}
          onCancel={() => setEditing(null)}
        />
      )}
      {editing?.provider && current && <GroupsEditor key={current.id} provider={current} onSaved={refresh} />}
      {!editing && (
        <div className={styles.buttons}>
          <Button variant="k" onClick={() => setEditing({ kind: "oidc", provider: null })}>
            {t("signIn.addOidc")}
          </Button>
          <Button onClick={() => setEditing({ kind: "ldap", provider: null })}>{t("signIn.addLdap")}</Button>
        </div>
      )}
    </div>
  );
}

function ProviderRow({
  provider,
  open,
  onEdit,
  onRemoved,
}: {
  provider: IdentityProvider;
  open: boolean;
  onEdit: () => void;
  onRemoved: () => void;
}) {
  return (
    <section className={styles.panel} aria-label={provider.name}>
      <div className={styles.sectionHead}>
        <h2 className={styles.sectionTitle}>{provider.name}</h2>
        <span className={styles.dim}>{t(`signIn.kind.${provider.kind}`)}</span>
        <Label tone={provider.enabled ? "ok" : "neutral"}>{provider.enabled ? t("signIn.on") : t("signIn.off")}</Label>
        <span className={styles.dim}>{t("signIn.accounts", { count: provider.accounts })}</span>
      </div>
      <div className={styles.buttons}>
        <Button aria-expanded={open} onClick={onEdit}>
          {t("signIn.edit")}
        </Button>
        <InlineConfirm
          layout="row"
          trigger={t("signIn.remove")}
          title={t("signIn.removeTitle", { name: provider.name })}
          consequence={t("signIn.removeConsequence", { count: provider.accounts })}
          confirmLabel={t("signIn.remove")}
          describeError={describeError}
          onConfirm={async () => {
            await api.removeIdentityProvider(provider.id);
            onRemoved();
          }}
        />
      </div>
    </section>
  );
}

type TestState = IdentityProviderTestResult | "testing" | null;

function ProviderForm({
  kind,
  provider,
  list,
  onSaved,
  onCancel,
}: {
  kind: IdentityProviderKind;
  provider: IdentityProvider | null;
  list: IdentityProviderList;
  onSaved: (provider: IdentityProvider) => void;
  onCancel: () => void;
}) {
  const titleId = useId();
  const [name, setName] = useState(provider?.name ?? "");
  const [settings, setSettings] = useState<IdentityProviderSettings>(
    provider?.settings ?? (kind === "oidc" ? OIDC_DEFAULTS : LDAP_DEFAULTS),
  );
  const [secret, setSecret] = useState("");
  const [person, setPerson] = useState({ username: "", password: "" });
  const [test, setTest] = useState<TestState>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const enabled = provider?.enabled ?? false;
  const passed = test !== null && test !== "testing" && test.ok;

  function changed() {
    setTest(null);
    setError(null);
    setSaved(false);
  }

  function update(patch: Partial<OidcSettings> | Partial<LdapSettings>) {
    setSettings((s) => ({ ...s, ...patch }) as IdentityProviderSettings);
    changed();
  }

  async function runTest() {
    setTest("testing");
    try {
      const withPerson = settings.kind === "ldap" && person.username.trim() !== "";
      setTest(
        await api.testIdentityProvider({
          id: provider?.id,
          secret: secret || null,
          settings,
          ...(withPerson && { username: person.username, password: person.password }),
        }),
      );
    } catch (e) {
      setTest({
        ok: false,
        error: e instanceof ApiError && e.status === 400 ? e.message : describeError(e),
        oidc: null,
        ldap: null,
        roles: [],
      });
    }
  }

  async function save(on: boolean) {
    setBusy(true);
    setError(null);
    const body = { name, enabled: on, secret: secret || null, settings };
    try {
      const result = provider
        ? await api.updateIdentityProvider(provider.id, body)
        : await api.createIdentityProvider(body);
      setSecret("");
      setSaved(true);
      onSaved(result);
    } catch (e) {
      setError(e instanceof ApiError && (e.status === 400 || e.status === 409) ? e.message : describeError(e));
    }
    setBusy(false);
  }

  function submit(e: FormEvent) {
    e.preventDefault();
    void save(enabled);
  }

  const secretPlaceholder = provider?.has_secret ? t("signIn.secretSaved") : "";
  const title = provider ? provider.name : kind === "oidc" ? t("signIn.newOidc") : t("signIn.newLdap");

  return (
    <form className={styles.panel} onSubmit={submit} aria-labelledby={titleId}>
      <h2 id={titleId} className={styles.sectionTitle}>
        {title}
      </h2>
      <label className={styles.field}>
        {t("signIn.name")}
        <input
          className={fieldClass}
          value={name}
          required
          maxLength={64}
          placeholder={kind === "oidc" ? "Keycloak" : "Active Directory"}
          onChange={(e) => {
            setName(e.target.value);
            changed();
          }}
        />
        <span className={styles.fieldNote}>{t("signIn.nameNote", { name: name.trim() || "Keycloak" })}</span>
      </label>
      {settings.kind === "oidc" ? (
        <OidcFields settings={settings} update={update} list={list} />
      ) : (
        <LdapFields settings={settings} update={update} />
      )}
      <label className={styles.field}>
        {settings.kind === "oidc" ? t("signIn.clientSecret") : t("signIn.bindPassword")}
        <input
          className={fieldClass}
          type="password"
          autoComplete="new-password"
          value={secret}
          placeholder={secretPlaceholder}
          onChange={(e) => {
            setSecret(e.target.value);
            changed();
          }}
        />
        <span className={styles.fieldNote}>
          {settings.kind === "oidc" ? t("signIn.clientSecretNote") : t("signIn.bindPasswordNote")}
        </span>
      </label>
      {settings.kind === "ldap" && (
        <fieldset className={styles.fieldset}>
          <legend className={styles.legend}>{t("signIn.tryPerson")}</legend>
          <div className={styles.formGrid}>
            <label className={styles.field}>
              {t("auth.username")}
              <input
                className={fieldClass}
                autoComplete="off"
                value={person.username}
                onChange={(e) => {
                  setPerson({ ...person, username: e.target.value });
                  setTest(null);
                }}
              />
            </label>
            <label className={styles.field}>
              {t("auth.password")}
              <input
                className={fieldClass}
                type="password"
                autoComplete="new-password"
                value={person.password}
                onChange={(e) => {
                  setPerson({ ...person, password: e.target.value });
                  setTest(null);
                }}
              />
            </label>
          </div>
          <span className={styles.fieldNote}>{t("signIn.tryPersonNote")}</span>
        </fieldset>
      )}
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
      <div className={styles.formFoot}>
        <Button disabled={test === "testing"} onClick={() => void runTest()}>
          {test === "testing" ? t("signIn.testing") : t("signIn.test")}
        </Button>
        {enabled ? (
          <>
            <Button type="submit" variant="k" disabled={busy}>
              {t("signIn.save")}
            </Button>
            <Button disabled={busy} onClick={() => void save(false)}>
              {t("signIn.turnOff")}
            </Button>
          </>
        ) : (
          <>
            <Button variant="k" disabled={busy || !passed} onClick={() => void save(true)}>
              {t("signIn.saveOn")}
            </Button>
            <Button type="submit" disabled={busy}>
              {t("signIn.save")}
            </Button>
          </>
        )}
        <Button disabled={busy} onClick={onCancel}>
          {t("signIn.cancel")}
        </Button>
        {saved && <span role="status">{t("signIn.saved")}</span>}
      </div>
      {!enabled && !passed && <span className={styles.fieldNote}>{t("signIn.testFirst")}</span>}
      <TestLine test={test} />
    </form>
  );
}

function OidcFields({
  settings,
  update,
  list,
}: {
  settings: OidcSettings;
  update: (patch: Partial<OidcSettings>) => void;
  list: IdentityProviderList;
}) {
  return (
    <>
      <label className={styles.field}>
        {t("signIn.issuer")}
        <input
          className={fieldClass}
          type="url"
          inputMode="url"
          required
          spellCheck={false}
          value={settings.issuer}
          placeholder="https://id.example.com/realms/acme"
          onChange={(e) => update({ issuer: e.target.value })}
        />
        <span className={styles.fieldNote}>{t("signIn.issuerNote")}</span>
      </label>
      <label className={styles.field}>
        {t("signIn.clientId")}
        <input
          className={fieldClass}
          required
          spellCheck={false}
          value={settings.client_id}
          placeholder="monitorflow"
          onChange={(e) => update({ client_id: e.target.value })}
        />
      </label>
      <div className={styles.field}>
        {t("signIn.redirectUri")}
        <div className={styles.buttonRow}>
          <code className={styles.code}>{list.redirect_uri}</code>
          <CopyButton text={list.redirect_uri} name={t("signIn.redirectUri")} />
        </div>
        <span className={styles.fieldNote}>{t("signIn.redirectUriNote")}</span>
      </div>
      <details>
        <summary>{t("signIn.advanced")}</summary>
        <div className={styles.stack}>
          <label className={styles.field}>
            {t("signIn.scopes")}
            <input
              className={fieldClass}
              spellCheck={false}
              value={settings.scopes}
              onChange={(e) => update({ scopes: e.target.value })}
            />
          </label>
          <label className={styles.field}>
            {t("signIn.groupsClaim")}
            <input
              className={fieldClass}
              spellCheck={false}
              value={settings.groups_claim}
              onChange={(e) => update({ groups_claim: e.target.value })}
            />
            <span className={styles.fieldNote}>{t("signIn.groupsClaimNote")}</span>
          </label>
          <CaField value={settings.ca_pem} onChange={(ca_pem) => update({ ca_pem })} />
        </div>
      </details>
    </>
  );
}

function CaField({ value, onChange }: { value: string | null; onChange: (pem: string | null) => void }) {
  return (
    <label className={styles.field}>
      {t("signIn.ca")}
      <textarea
        className={cx(fieldClass, styles.mono)}
        rows={4}
        spellCheck={false}
        value={value ?? ""}
        placeholder="-----BEGIN CERTIFICATE-----"
        onChange={(e) => onChange(e.target.value.trim() ? e.target.value : null)}
      />
      <span className={styles.fieldNote}>{t("signIn.caNote")}</span>
    </label>
  );
}

function LdapFields({
  settings,
  update,
}: {
  settings: LdapSettings;
  update: (patch: Partial<LdapSettings>) => void;
}) {
  const preset = settings.user_filter === PRESETS.ad.user_filter ? "ad" : "openldap";
  const text = (key: "bind_dn" | "user_base_dn", label: string, placeholder: string, note?: string) => (
    <label className={styles.field}>
      {label}
      <input
        className={fieldClass}
        required
        spellCheck={false}
        value={settings[key]}
        placeholder={placeholder}
        onChange={(e) => update({ [key]: e.target.value })}
      />
      {note && <span className={styles.fieldNote}>{note}</span>}
    </label>
  );
  return (
    <>
      <fieldset className={styles.fieldset}>
        <legend className={styles.legend}>{t("signIn.preset")}</legend>
        <div className={styles.radios}>
          {(["ad", "openldap"] as const).map((p) => (
            <label key={p}>
              <input type="radio" name="preset" checked={preset === p} onChange={() => update({ ...PRESETS[p] })} />{" "}
              {t(`signIn.preset.${p}`)}
            </label>
          ))}
        </div>
      </fieldset>
      <label className={styles.field}>
        {t("signIn.url")}
        <input
          className={fieldClass}
          required
          spellCheck={false}
          value={settings.url}
          placeholder="ldaps://ldap.example.com"
          onChange={(e) => {
            const url = e.target.value;
            // The address says which encryption: ldaps:// is LDAPS, ldap:// needs StartTLS.
            update({ url, starttls: url.trim().toLowerCase().startsWith("ldap://") });
          }}
        />
        <span className={styles.fieldNote}>{t("signIn.urlNote")}</span>
      </label>
      <fieldset className={styles.fieldset}>
        <legend className={styles.legend}>{t("signIn.security")}</legend>
        <div className={styles.radios}>
          <label>
            <input
              type="radio"
              name="security"
              checked={!settings.starttls}
              onChange={() => update({ starttls: false, url: settings.url.replace(/^ldap:\/\//i, "ldaps://") })}
            />{" "}
            {t("signIn.ldaps")}
          </label>
          <label>
            <input
              type="radio"
              name="security"
              checked={settings.starttls}
              onChange={() => update({ starttls: true, url: settings.url.replace(/^ldaps:\/\//i, "ldap://") })}
            />{" "}
            {t("signIn.starttls")}
          </label>
        </div>
      </fieldset>
      {text("bind_dn", t("signIn.bindDn"), "cn=monitorflow,ou=services,dc=example,dc=com", t("signIn.bindDnNote"))}
      {text("user_base_dn", t("signIn.userBase"), "ou=people,dc=example,dc=com")}
      <label className={styles.field}>
        {t("signIn.groupBase")}
        <input
          className={fieldClass}
          spellCheck={false}
          value={settings.group_base_dn ?? ""}
          placeholder="ou=groups,dc=example,dc=com"
          onChange={(e) => update({ group_base_dn: e.target.value || null })}
        />
        <span className={styles.fieldNote}>{t("signIn.groupBaseNote")}</span>
      </label>
      <label className={styles.check}>
        <input
          type="checkbox"
          checked={settings.nested_groups}
          onChange={(e) => update({ nested_groups: e.target.checked })}
        />{" "}
        {t("signIn.nested")}
      </label>
      <details>
        <summary>{t("signIn.advanced")}</summary>
        <div className={styles.stack}>
          <label className={styles.field}>
            {t("signIn.userFilter")}
            <input
              className={cx(fieldClass, styles.mono)}
              spellCheck={false}
              value={settings.user_filter}
              onChange={(e) => update({ user_filter: e.target.value })}
            />
          </label>
          <label className={styles.field}>
            {t("signIn.groupFilter")}
            <input
              className={cx(fieldClass, styles.mono)}
              spellCheck={false}
              value={settings.group_filter}
              onChange={(e) => update({ group_filter: e.target.value })}
            />
          </label>
          <label className={styles.field}>
            {t("signIn.groupName")}
            <input
              className={fieldClass}
              spellCheck={false}
              value={settings.group_name_attribute}
              onChange={(e) => update({ group_name_attribute: e.target.value })}
            />
          </label>
          <CaField value={settings.ca_pem} onChange={(ca_pem) => update({ ca_pem })} />
        </div>
      </details>
    </>
  );
}

function TestLine({ test }: { test: TestState }) {
  const multiCustomer = useMultiCustomer();
  if (test === null || test === "testing") return null;
  if (!test.ok) {
    return (
      <p role="alert">
        <Label tone="bad">{t("signIn.failing")}</Label> {test.error}
      </p>
    );
  }
  const lines: string[] = [];
  if (test.oidc) {
    lines.push(t("signIn.foundOidc", { issuer: test.oidc.issuer, count: test.oidc.keys }));
    lines.push(test.oidc.client_checked ? t("signIn.clientChecked") : t("signIn.publicClient"));
  }
  if (test.ldap) {
    lines.push(test.ldap.active_directory ? t("signIn.foundAd") : t("signIn.foundLdap"));
    if (test.ldap.dn) {
      lines.push(
        test.ldap.groups.length > 0
          ? t("signIn.personGroups", { dn: test.ldap.dn, groups: test.ldap.groups.join(", ") })
          : t("signIn.personNoGroups", { dn: test.ldap.dn }),
      );
      lines.push(
        test.roles.length > 0
          ? t("signIn.wouldBe", { roles: test.roles.map((g) => describeGrant(g, undefined, multiCustomer)).join(", ") })
          : t("signIn.wouldNot"),
      );
    }
  }
  return (
    <div role="status" className={styles.stack}>
      <span>
        <Label tone="ok">{t("signIn.works")}</Label>
      </span>
      {lines.map((line) => (
        <span key={line} className={styles.dim}>
          {line}
        </span>
      ))}
    </div>
  );
}

/** "Groups and roles" of a saved provider, saved as a whole. */
function GroupsEditor({ provider, onSaved }: { provider: IdentityProvider; onSaved: () => void }) {
  const titleId = useId();
  const multiCustomer = useMultiCustomer();
  const namesFetcher = useCallback(
    async (signal: AbortSignal) => {
      if (!multiCustomer) return null;
      const [{ customers }, { sites }] = await Promise.all([api.customers(signal), api.sites(signal)]);
      return { customers, sites };
    },
    [multiCustomer],
  );
  const { data: names } = usePolledData(namesFetcher);
  const [rows, setRows] = useState<GroupMapping[]>(provider.mappings);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  function edit(index: number, patch: Partial<GroupMapping>) {
    setRows(rows.map((row, i) => (i === index ? { ...row, ...patch } : row)));
    setSaved(false);
    setError(null);
  }

  async function save(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const result = await api.setIdentityProviderGroups(
        provider.id,
        rows.filter((r) => r.group.trim() !== ""),
      );
      setRows(result.mappings);
      setSaved(true);
      onSaved();
    } catch (err) {
      setError(err instanceof ApiError && err.status === 400 ? err.message : describeError(err));
    }
    setBusy(false);
  }

  const scopeValue = (row: GroupMapping) =>
    row.site_id ? `s:${row.site_id}` : row.customer_id ? `c:${row.customer_id}` : "";
  const scopeOf = (value: string): Pick<GroupMapping, "customer_id" | "site_id"> =>
    value.startsWith("c:")
      ? { customer_id: value.slice(2), site_id: null }
      : value.startsWith("s:")
        ? { customer_id: null, site_id: value.slice(2) }
        : { customer_id: null, site_id: null };

  return (
    <form className={styles.panel} onSubmit={save} aria-labelledby={titleId}>
      <h2 id={titleId} className={styles.sectionTitle}>
        {t("signIn.groups")}
      </h2>
      <p className={styles.sectionNote}>{t("signIn.groupsNote")}</p>
      {rows.length === 0 && <Banner tone="warn">{t("signIn.noGroups")}</Banner>}
      {rows.map((row, index) => (
        <div key={index} className={styles.formGrid}>
          <label className={styles.field}>
            {t("signIn.group")}
            <input
              className={fieldClass}
              spellCheck={false}
              value={row.group}
              placeholder={t("signIn.groupPlaceholder")}
              onChange={(e) => edit(index, { group: e.target.value })}
            />
          </label>
          <label className={styles.field}>
            {t("signIn.role")}
            <select className={fieldClass} value={row.role} onChange={(e) => edit(index, { role: e.target.value as Role })}>
              {ROLES.map((role) => (
                <option key={role} value={role}>
                  {t(`common.role.${role}`)}
                </option>
              ))}
            </select>
          </label>
          {/* A single company has one place: no customer or site to pick (ADR-0104). */}
          {multiCustomer && (
            <label className={styles.field}>
              {t("signIn.where")}
              <select
                className={fieldClass}
                value={scopeValue(row)}
                onChange={(e) => edit(index, scopeOf(e.target.value))}
              >
                <option value="">{t("signIn.everything")}</option>
                {names?.customers.map((customer) => (
                  <optgroup key={customer.id} label={customer.name}>
                    <option value={`c:${customer.id}`}>{t("users.allOf", { name: customer.name })}</option>
                    {names.sites
                      .filter((site) => site.customer_id === customer.id)
                      .map((site) => (
                        <option key={site.id} value={`s:${site.id}`}>
                          {t("users.siteOption", { name: site.name })}
                        </option>
                      ))}
                  </optgroup>
                ))}
              </select>
            </label>
          )}
          <div className={styles.field}>
            <span aria-hidden="true">&nbsp;</span>
            <Button onClick={() => setRows(rows.filter((_, i) => i !== index))}>{t("signIn.remove")}</Button>
          </div>
        </div>
      ))}
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
      <div className={styles.formFoot}>
        <Button
          onClick={() => {
            setRows([...rows, { group: "", role: "viewer", customer_id: null, site_id: null }]);
            setSaved(false);
          }}
        >
          {t("signIn.addGroup")}
        </Button>
        <Button type="submit" variant="k" disabled={busy}>
          {t("signIn.saveGroups")}
        </Button>
        {saved && <span role="status">{t("signIn.saved")}</span>}
      </div>
    </form>
  );
}
