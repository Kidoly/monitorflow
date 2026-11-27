"use client";

import Link from "next/link";
import { type FormEvent, type RefObject, useEffect, useId, useRef, useState } from "react";

import { Button, ButtonLink, fieldClass } from "@/components/ui/Button";
import { InfoTip } from "@/components/ui/Hint";
import { cx } from "@/components/ui/cx";
import { type Column, DataTable } from "@/components/ui/DataTable";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { Banner, EmptyState, ErrorState, NoPermission, PageHeader, StaleBanner } from "@/components/ui/PageState";
import { CopyButton, SecretField } from "@/components/ui/SecretField";
import { Label, StatusLabel } from "@/components/ui/StatusLabel";
import { formatAgo } from "@/components/ui/time";
import { Switch } from "@/components/ui/Switch";
import { SegmentedControl } from "@/components/ui/SegmentedControl";
import { rich, t } from "@/i18n";
import {
  type AgentInstall,
  type CreatedInstallKey,
  type CreatedToken,
  type EnrollmentToken,
  type InstallSource,
  type Me,
  type PublicAddress,
  type Site,
  type TokenRole,
  api,
  describeError,
} from "@/lib/api";
import { formatWhen } from "@/lib/clock";
import {
  type Command,
  type InstallTarget,
  SOURCES,
  isSafeUrl,
  isServerAddress,
  sourceWord,
  linuxInstallCommand,
  relayInstallCommand,
  windowsInstallCommand,
} from "@/lib/install";
import { placeLabel } from "@/lib/customers";
import { administersCustomer, administersEverything, multiCustomer } from "@/lib/me";
import { usePolledData } from "@/lib/usePolledData";

import { AdminLayout } from "./AdminLayout";
import { SERVER_UPDATE_PATH, SITES_PATH } from "@/lib/routes";
import styles from "./AdminPages.module.css";
import { CreateInstallKey, InstallKeyTable, PendingHosts } from "./InstallKeys";

/** Validity choices; the API takes 60 s to 7 days. */
const TTLS = [
  [900, "install.ttl.15m"],
  [3600, "install.ttl.1h"],
  [86_400, "install.ttl.1d"],
  [604_800, "install.ttl.7d"],
] as const;

const roleWord = (role: TokenRole) => t(`install.role.${role}`);

/**
 * `/settings/add-servers/`: the site's reusable install command first, with hosts
 * waiting for approval (ADR-0073); single-use tokens under Advanced
 * (ADR-0029).
 */
export function TokensAdminPage() {
  return <AdminLayout>{(me) => <TokensAdmin me={me} />}</AdminLayout>;
}

async function load(signal: AbortSignal) {
  const [list, keys, { sites }] = await Promise.all([api.tokens(signal), api.installKeys(signal), api.sites(signal)]);
  return { tokens: list.tokens, more: list.next_cursor !== null, sites, ...keys };
}

export function TokensAdmin({ me }: { me: Me }) {
  const allowed = administersCustomer(me);
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(load, { enabled: allowed });
  // Held in memory only, until dismissed or the page is left: the API never returns them again.
  const [created, setCreated] = useState<CreatedToken | null>(null);
  const [createdKey, setCreatedKey] = useState<CreatedInstallKey | null>(null);
  const [advanced, setAdvanced] = useState(false);
  const peerRef = useRef<HTMLInputElement>(null);
  const advancedId = useId();

  if (!allowed) {
    return (
      multiCustomer(me) ? (
        <NoPermission title={t("install.noPermission.customer")}>{t("install.noPermission.customerNote")}</NoPermission>
      ) : (
        <NoPermission title={t("install.noPermission.allSites")}>{t("install.noPermission.allSitesNote")}</NoPermission>
      )
    );
  }
  const administered = data?.sites.filter((site) => administersCustomer(me, site.customer_id)) ?? [];
  return (
    <div className={styles.page}>
      <PageHeader
        title={t("install.title")}
        status={<InfoTip label={t("install.about")}>{t("install.meta")}</InfoTip>}
      />
      {stale && updatedAt && (
        <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />
      )}
      {!data && error ? (
        <ErrorState what={t("install.what")} error={describeError(error)} onRetry={refresh} />
      ) : (
        <>
          {data && administered.length === 0 ? (
            <EmptyState
              title={t("install.noSite.title")}
              actions={
                <ButtonLink variant="k" href={SITES_PATH}>
                  {t("install.noSite.create")}
                </ButtonLink>
              }
            >
              {t("install.noSite.note")}
            </EmptyState>
          ) : createdKey ? (
            <NewInstallKey
              installKey={createdKey}
              editable={administersEverything(me)}
              onDone={() => setCreatedKey(null)}
            />
          ) : (
            <CreateInstallKey
              sites={administered}
              onCreated={(key) => {
                setCreatedKey(key);
                refresh();
              }}
            />
          )}
          <PendingHosts hosts={data?.pending_hosts ?? []} now={updatedAt ?? new Date()} onChange={refresh} />
          <InstallKeyTable keys={data?.keys} now={updatedAt ?? new Date()} onChange={refresh} />
          <section className={styles.section} aria-labelledby={advancedId}>
            <div className={styles.sectionHead}>
              <h2 id={advancedId} className={styles.sectionTitle}>
                {t("install.advanced")}
              </h2>
              <InfoTip label={t("install.about")}>{t("install.tokens.note")}</InfoTip>
              <Button size="small" aria-expanded={advanced} onClick={() => setAdvanced(!advanced)}>
                {advanced ? t("install.tokens.hide") : t("install.tokens.show")}
              </Button>
            </div>
          </section>
          {advanced && (
            <>
              {data && administered.length > 0 && (
                <CreateToken
                  me={me}
                  sites={administered}
                  peerRef={peerRef}
                  onCreated={(token) => {
                    setCreated(token);
                    refresh();
                  }}
                />
              )}
              {created && (
                <NewToken
                  token={created}
                  editable={administersEverything(me)}
                  onDone={() => setCreated(null)}
                  onAnother={() => {
                    setCreated(null);
                    peerRef.current?.focus();
                  }}
                />
              )}
              <TokenTable
                tokens={data?.tokens}
                more={data?.more ?? false}
                now={updatedAt ?? new Date()}
                onChange={refresh}
              />
            </>
          )}
          <DefaultInstallSource editable={administersEverything(me)} />
        </>
      )}
    </div>
  );
}

function CreateToken({
  me,
  sites,
  peerRef,
  onCreated,
}: {
  me: Me;
  sites: Site[];
  peerRef: RefObject<HTMLInputElement | null>;
  onCreated: (token: CreatedToken) => void;
}) {
  const titleId = useId();
  const [role, setRole] = useState<TokenRole>("agent");
  const [peerId, setPeerId] = useState("");
  const [siteId, setSiteId] = useState("");
  const [ttl, setTtl] = useState(3600);
  const [names, setNames] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const token = await api.createToken({
        role,
        peer_id: peerId.trim(),
        site_id: siteId || (sites[0]?.id ?? ""),
        ttl_s: ttl,
        server_names: role === "relay" ? names.split(/[\s,]+/).filter(Boolean) : [],
      });
      setPeerId("");
      setNames("");
      onCreated(token);
    } catch (e) {
      setError(t("install.token.createFailed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className={styles.panel} onSubmit={submit} aria-labelledby={titleId}>
      <h2 id={titleId} className={styles.sectionTitle}>
        {t("install.token.new")}
      </h2>
      <div className={styles.formGrid}>
        <fieldset className={styles.fieldset}>
          <legend className={styles.legend}>{t("install.token.role")}</legend>
          <SegmentedControl
            label={t("install.token.role")}
            options={(["agent", "relay"] as const).map((value) => ({ value, label: roleWord(value) }))}
            value={role}
            onChange={setRole}
          />
        </fieldset>
        <label className={styles.field}>
          {t("install.token.name")}
          <input
            ref={peerRef}
            className={fieldClass}
            value={peerId}
            onChange={(e) => setPeerId(e.target.value)}
            placeholder={t("install.token.namePlaceholder")}
            required
            pattern="[A-Za-z0-9._\-]{1,64}"
            title={t("install.token.nameRule")}
            spellCheck={false}
            autoCapitalize="none"
          />
        </label>
        <label className={styles.field}>
          {t("install.token.site")}
          <select className={fieldClass} value={siteId || sites[0]?.id} onChange={(e) => setSiteId(e.target.value)}>
            {sites.map((site) => (
              <option key={site.id} value={site.id}>
                {placeLabel(site.customer_name, site.name, multiCustomer(me))}
              </option>
            ))}
          </select>
        </label>
        <label className={styles.field}>
          {t("install.token.expiresIn")}
          <select className={fieldClass} value={ttl} onChange={(e) => setTtl(Number(e.target.value))}>
            {TTLS.map(([seconds, label]) => (
              <option key={seconds} value={seconds}>
                {t(label)}
              </option>
            ))}
          </select>
        </label>
      </div>
      <label className={styles.field}>
        <span>
          {t("install.token.addresses")} <span className={styles.fieldNote}>{t("install.token.addressesNote")}</span>
        </span>
        <input
          className={fieldClass}
          value={role === "relay" ? names : ""}
          onChange={(e) => setNames(e.target.value)}
          placeholder="relay1.paris.example 10.0.0.5"
          required={role === "relay"}
          disabled={role !== "relay"}
          spellCheck={false}
          autoCapitalize="none"
        />
      </label>
      <div className={styles.formFoot}>
        <Button variant="k" type="submit" disabled={busy || sites.length === 0}>
          {t("install.token.create")}
        </Button>
      </div>
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
    </form>
  );
}

/** The configuration a peer needs, as in `deploy/config/{agent,relay}.example.toml`. */
export function configSnippet(token: CreatedToken): string {
  return [
    `upstream_url = "${token.upstream_url}"`,
    `enroll_url = "${token.enroll_url}"`,
    `enrollment_token = "${token.token}"`,
  ].join("\n");
}

export function NewToken({
  token: created,
  editable,
  onDone,
  onAnother,
}: {
  token: CreatedToken;
  editable: boolean;
  onDone: () => void;
  onAnother: () => void;
}) {
  const titleId = useId();
  const snippetId = useId();
  // Changing the address rewrites the snippet and every command at once.
  const [address, setAddress] = useState(created.address);
  const token = { ...created, upstream_url: address.upstream_url, enroll_url: address.enroll_url };
  const snippet = configSnippet(token);
  const file = token.role === "relay" ? "/etc/monitorflow/relay.toml" : "/etc/monitorflow/agent.toml";
  return (
    <section className={cx(styles.panel, styles.created)} aria-labelledby={titleId} aria-live="polite">
      <div className={styles.panelTitle}>
        <Label tone="ok">{t("install.created")}</Label>
        <h2 id={titleId} className={styles.sectionTitle}>
          {t("install.newToken.title", { name: token.peer_id, when: formatWhen(new Date(token.expires_at), new Date()) })}
        </h2>
      </div>
      <Banner tone="warn" role="alert">
        <strong>{t("install.newToken.once")}</strong>{" "}
        {token.role === "relay" ? t("install.newToken.onceRelay") : t("install.newToken.onceAgent")}
      </Banner>
      <ServerAddress value={address} editable={editable} onChange={setAddress} />
      <div className={styles.createdGrid}>
        <div className={styles.stack}>
          <SecretField label={t("install.newToken.token")} value={token.token} copyName={t("install.newToken.copy")} />
          <div className={styles.buttons}>
            <Button variant="k" onClick={onDone}>
              {t("install.done")}
            </Button>
            <Button onClick={onAnother}>{t("install.newToken.another")}</Button>
          </div>
        </div>
        <div className={styles.field}>
          <span id={snippetId}>
            {t("install.newToken.snippet")} <span className={styles.fieldNote}>({file})</span>
          </span>
          <div className={styles.snippet}>
            <pre className={styles.code} aria-labelledby={snippetId}>
              {snippet}
            </pre>
            <CopyButton text={snippet} name={t("install.newToken.copySnippet")} size="small" className={styles.snippetCopy} />
          </div>
          {token.role === "agent" && (
            <span className={styles.fieldNote}>
              {rich("install.newToken.env", {
                variable: <code>MONITORFLOW_ENROLLMENT_TOKEN</code>,
                key: <code>upstream_url</code>,
              })}
            </span>
          )}
        </div>
      </div>
      {token.role === "agent" ? <InstallCommands token={token} /> : <RelayInstallCommand target={token} />}
    </section>
  );
}

/** A new install command, shown once with the address it connects to, through `relay` if given. */
export function NewInstallKey({
  installKey: created,
  editable,
  relay = "",
  onDone,
}: {
  installKey: CreatedInstallKey;
  editable: boolean;
  relay?: string;
  onDone: () => void;
}) {
  const titleId = useId();
  const [address, setAddress] = useState(created.address);
  const key = { ...created, upstream_url: address.upstream_url, enroll_url: address.enroll_url };
  return (
    <section className={cx(styles.panel, styles.created)} aria-labelledby={titleId} aria-live="polite">
      <div className={styles.panelTitle}>
        <Label tone="ok">{t("install.created")}</Label>
        <h2 id={titleId} className={styles.sectionTitle}>
          {t("install.newKey.title", { site: key.site_name, when: formatWhen(new Date(key.expires_at), new Date()) })}
        </h2>
      </div>
      <Banner tone="warn" role="alert">
        <strong>{t("install.newKey.once")}</strong>{" "}
        {key.max_hosts === null
          ? key.require_approval
            ? t("install.newKey.anyApproval")
            : t("install.newKey.any")
          : key.require_approval
            ? t("install.newKey.upToApproval", { count: key.max_hosts })
            : t("install.newKey.upTo", { count: key.max_hosts })}
      </Banner>
      <ServerAddress value={address} editable={editable} onChange={setAddress} />
      <InstallCommands token={key} relay={relay} />
      <div className={styles.buttons}>
        <Button variant="k" onClick={onDone}>
          {t("install.done")}
        </Button>
      </div>
    </section>
  );
}

/** A new relay's command, shown once: the key works on one host, and the relay then waits to be confirmed. */
export function NewRelayKey({
  installKey: created,
  editable,
  onDone,
}: {
  installKey: CreatedInstallKey;
  editable: boolean;
  onDone: () => void;
}) {
  const titleId = useId();
  const [address, setAddress] = useState(created.address);
  const key = { ...created, upstream_url: address.upstream_url, enroll_url: address.enroll_url };
  return (
    <section className={cx(styles.panel, styles.created)} aria-labelledby={titleId} aria-live="polite">
      <div className={styles.panelTitle}>
        <Label tone="ok">{t("install.created")}</Label>
        <h2 id={titleId} className={styles.sectionTitle}>
          {t("install.newRelay.title", { site: key.site_name, when: formatWhen(new Date(key.expires_at), new Date()) })}
        </h2>
      </div>
      <Banner tone="warn" role="alert">
        <strong>{t("install.newRelay.once")}</strong> {t("install.newRelay.note")}
      </Banner>
      <ServerAddress value={address} editable={editable} onChange={setAddress} />
      <RelayInstallCommand target={key} />
      <div className={styles.buttons}>
        <Button variant="k" onClick={onDone}>
          {t("install.done")}
        </Button>
      </div>
    </section>
  );
}

const OTHER_ADDRESS = "other";

/** This page's host name, which hosts often reach the server at too; loopback names are left out. */
function pageHost(): string | null {
  if (typeof window === "undefined") return null;
  const host = window.location.hostname.replace(/^\[(.*)\]$/, "$1");
  return host && !/^(localhost|127\.[\d.]+|::1)$/i.test(host) ? host : null;
}

/**
 * The address the snippet and commands connect to: one of the certificate's
 * names, this page's host or a typed one. Choosing a name applies it at once.
 */
export function ServerAddress({
  value,
  editable,
  onChange,
  note = t("install.address.note"),
}: {
  value: PublicAddress;
  editable: boolean;
  onChange: (address: PublicAddress) => void;
  note?: string;
}) {
  const labelId = useId();
  const [typing, setTyping] = useState(false);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const certified = (name: string) => value.names.some((n) => n.toLowerCase() === name.toLowerCase());
  const choices = [value.address, ...value.names.filter((n) => !n.startsWith("*.")), pageHost() ?? ""].filter(
    (name, i, all) => name && all.findIndex((n) => n.toLowerCase() === name.toLowerCase()) === i,
  );

  async function save(address: string) {
    setBusy(true);
    setError(null);
    try {
      onChange(await api.setPublicAddress({ address }));
      setTyping(false);
      setDraft("");
    } catch (e) {
      setError(t("install.address.failed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className={styles.field}>
      <span id={labelId}>
        {t("install.address.label")} <span className={styles.fieldNote}>{note}</span>
      </span>
      {editable ? (
        <div className={styles.buttons}>
          <select
            className={fieldClass}
            aria-labelledby={labelId}
            value={typing ? OTHER_ADDRESS : value.address}
            disabled={busy}
            onChange={(e) => {
              if (e.target.value === OTHER_ADDRESS) setTyping(true);
              else void save(e.target.value);
            }}
          >
            {choices.map((name) => (
              <option key={name} value={name}>
                {name}
                {name === value.default
                  ? t("install.address.default")
                  : certified(name)
                    ? ""
                    : t("install.address.notCertified")}
              </option>
            ))}
            <option value={OTHER_ADDRESS}>{t("install.address.other")}</option>
          </select>
          {typing && (
            <form
              className={styles.buttons}
              onSubmit={(e) => {
                e.preventDefault();
                void save(draft);
              }}
            >
              <input
                className={fieldClass}
                aria-label={t("install.address.otherLabel")}
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                placeholder="monitor.example.com or 10.0.0.2"
                spellCheck={false}
                autoCapitalize="none"
                autoFocus
              />
              <Button variant="k" type="submit" disabled={busy || draft.trim() === ""}>
                {t("install.address.use")}
              </Button>
            </form>
          )}
        </div>
      ) : (
        <span className={styles.fieldNote}>
          {rich("install.address.readOnly", { address: <code>{value.address}</code> })}
        </span>
      )}
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
      {!value.certified && (
        <Banner tone="warn">
          <strong>{t("install.address.refused", { address: value.address })}</strong>{" "}
          {rich(value.names.length > 0 ? "install.address.refusedWhyNames" : "install.address.refusedWhy", {
            list: value.names.join(", "),
            address: value.address,
            names: <code>SERVER_NAMES</code>,
            env: <code>.env</code>,
            key: <code>server_names</code>,
            file: <code>server.toml</code>,
          })}
        </Banner>
      )}
    </div>
  );
}

/** This page's origin, where the server's own release files are (ADR-0054). */
function pageOrigin(): string {
  return typeof window === "undefined" ? "" : window.location.origin;
}

function SourceChoice({
  label,
  value,
  onChange,
  disabled = false,
}: {
  label: string;
  value: InstallSource;
  onChange: (source: InstallSource) => void;
  disabled?: boolean;
}) {
  return (
    <SegmentedControl
      label={label}
      options={SOURCES.map((source) => ({ value: source, label: sourceWord(source) }))}
      value={value}
      onChange={onChange}
      disabled={disabled}
    />
  );
}

/**
 * Ready-to-paste install commands for an agent token or install key
 * (ADR-0052, ADR-0073), from a chosen source (ADR-0054), through `initialRelay` if given.
 */
function InstallCommands({ token, relay: initialRelay = "" }: { token: InstallTarget; relay?: string }) {
  const titleId = useId();
  const [source, setSource] = useState<InstallSource>(token.install.source);
  const [relay, setRelay] = useState(initialRelay);
  // Ticked by default (ADR-0175): the server's page can then turn AI
  // commands on; nothing runs until someone does.
  const [aiFromWeb, setAiFromWeb] = useState(true);
  // Off by default (ADR-0189): scripts run code of the central server's
  // people on the host, which its administrator must choose.
  const [scripts, setScripts] = useState(false);
  // Off by default (ADR-0182): a major upgrade is the change most likely
  // to leave a server that does not boot.
  const [majorFromWeb, setMajorFromWeb] = useState(false);
  // Unticked (ADR-0190): a terminal runs whatever is typed, so each host opts in.
  const [remoteTerminal, setRemoteTerminal] = useState(false);
  // Unticked too (ADR-0190): a desktop gives a whole Windows session, its own consent.
  const [remoteDesktop, setRemoteDesktop] = useState(false);
  // Off by default (ADR-0188): software from the catalogue, with winget, apt or dnf.
  const [softwareFromWeb, setSoftwareFromWeb] = useState(false);
  const valid = relay.trim() === "" || isServerAddress(relay);
  const through = valid ? relay : "";
  const origin = pageOrigin();
  const commands: { name: string; note: string; command: Command }[] = [
    {
      name: "Linux",
      note: t("install.linux.note"),
      command: linuxInstallCommand(
        token,
        source,
        origin,
        through,
        aiFromWeb,
        scripts,
        majorFromWeb,
        remoteTerminal,
        softwareFromWeb,
      ),
    },
    {
      name: "Windows",
      note: t("install.windows.note"),
      command: windowsInstallCommand(
        token,
        source,
        origin,
        through,
        aiFromWeb,
        scripts,
        remoteTerminal,
        softwareFromWeb,
        remoteDesktop,
      ),
    },
  ];
  // One note when both commands are missing for the same reason.
  const reasons = commands.flatMap(({ command }) => ("unavailable" in command ? [command.unavailable] : []));
  const shared = reasons.length === 2 && reasons[0] === reasons[1] ? reasons[0] : null;
  const central = token.install.central;
  return (
    <div className={styles.stack} role="group" aria-labelledby={titleId}>
      <h3 id={titleId} className={styles.sectionTitle}>
        {t("install.agent.title")}
      </h3>
      <fieldset className={styles.fieldset}>
        <legend className={styles.legend}>{t("install.downloadFrom")}</legend>
        <SourceChoice label={t("install.downloadFrom")} value={source} onChange={setSource} />
      </fieldset>
      {source === "central" && central && !shared && (
        <p className={styles.fieldNote}>
          {t("install.agent.central", { version: central.version })}
        </p>
      )}
      <label className={styles.field}>
        <span>
          {t("install.agent.relay")} <span className={styles.fieldNote}>{t("install.agent.relayNote")}</span>
        </span>
        <input
          className={fieldClass}
          value={relay}
          onChange={(e) => setRelay(e.target.value)}
          placeholder="relay1.paris.example:7443"
          aria-invalid={!valid}
          spellCheck={false}
          autoCapitalize="none"
        />
      </label>
      {!valid && (
        <p className={styles.formError} role="alert">
          {t("install.agent.relayInvalid")}
        </p>
      )}
      <Switch checked={aiFromWeb} onChange={setAiFromWeb} label={t("install.agent.aiFromWeb")} note={t("install.agent.aiFromWebNote")} />
      <Switch checked={scripts} onChange={setScripts} label={t("install.agent.scripts")} note={t("install.agent.scriptsNote")} />
      <Switch
        checked={majorFromWeb}
        onChange={setMajorFromWeb}
        label={t("install.agent.majorFromWeb")}
        note={t("install.agent.majorFromWebNote")}
      />
      <Switch
        checked={remoteTerminal}
        onChange={setRemoteTerminal}
        label={t("install.agent.remoteTerminal")}
        note={t("install.agent.remoteTerminalNote")}
      />
      <Switch
        checked={remoteDesktop}
        onChange={setRemoteDesktop}
        label={t("install.agent.remoteDesktop")}
        note={t("install.agent.remoteDesktopNote")}
      />
      <Switch
        checked={softwareFromWeb}
        onChange={setSoftwareFromWeb}
        label={t("install.agent.softwareFromWeb")}
        note={t("install.agent.softwareFromWebNote")}
      />
      {shared ? (
        <Banner tone="warn">{shared}</Banner>
      ) : (
        commands.map(({ name, note, command }) =>
          "text" in command ? (
            <InstallCommand key={name} name={name} note={note} text={command.text} />
          ) : (
            <Banner key={name} tone="warn">
              {t("install.command.unavailable", { name, reason: command.unavailable })}
            </Banner>
          ),
        )
      )}
    </div>
  );
}

/**
 * The relay's ready-to-paste install command (ADR-0055), from the chosen
 * source like the agent's, with mirroring as a checkbox (ADR-0066).
 */
function RelayInstallCommand({ target }: { target: InstallTarget }) {
  const titleId = useId();
  const [source, setSource] = useState<InstallSource>(target.install.source);
  const [mirror, setMirror] = useState(false);
  const command = relayInstallCommand(target, source, pageOrigin(), mirror);
  const central = target.install.central;
  return (
    <div className={styles.stack} role="group" aria-labelledby={titleId}>
      <h3 id={titleId} className={styles.sectionTitle}>
        {t("install.relay.title")}
      </h3>
      <fieldset className={styles.fieldset}>
        <legend className={styles.legend}>{t("install.downloadFrom")}</legend>
        <SourceChoice label={t("install.downloadFrom")} value={source} onChange={setSource} />
      </fieldset>
      {source === "central" && central && "text" in command && (
        <p className={styles.fieldNote}>
          {t("install.relay.central", { version: central.version })}
        </p>
      )}
      <Switch checked={mirror} onChange={setMirror} label={t("install.relay.mirror")} note={t("install.relay.mirrorNote")} />
      {"text" in command ? (
        <InstallCommand name="Linux" note={t("install.linux.note")} text={command.text} />
      ) : (
        <Banner tone="warn">{command.unavailable}</Banner>
      )}
      <p className={styles.fieldNote}>{t("install.relay.after")}</p>
    </div>
  );
}

/** The server-wide default source (ADR-0054); global administrators change it. */
export function DefaultInstallSource({ editable }: { editable: boolean }) {
  const titleId = useId();
  const [current, setCurrent] = useState<AgentInstall | null>(null);
  const [source, setSource] = useState<InstallSource>("github");
  const [customUrl, setCustomUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  function show(install: AgentInstall) {
    setCurrent(install);
    setSource(install.source);
    setCustomUrl(install.custom_url ?? "");
  }

  useEffect(() => {
    const controller = new AbortController();
    api
      .agentInstall(controller.signal)
      .then(show)
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setError(t("install.source.loadFailed", { error: describeError(e) }));
      });
    return () => controller.abort();
  }, []);

  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError(null);
    setSaved(false);
    try {
      show(await api.setAgentInstall({ source, custom_url: customUrl.trim() || null }));
      setSaved(true);
    } catch (e) {
      setError(t("install.source.saveFailed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  const url = customUrl.trim();
  const urlValid = url === "" || isSafeUrl(url);
  return (
    <form className={styles.section} onSubmit={submit} aria-labelledby={titleId}>
      <div className={styles.sectionHead}>
        <h2 id={titleId} className={styles.sectionTitle}>
          {t("install.source.title")}
        </h2>
      </div>
      {!editable && <p className={styles.sectionNote}>{t("install.source.readOnly")}</p>}
      <div className={styles.sectionBody}>
        {current && (
          <>
            <fieldset className={styles.fieldset} disabled={!editable || current.source_from_config}>
              <legend className={styles.legend}>
                {t("install.source.label")}{" "}
                {current.source_from_config && <span className={styles.fieldNote}>{t("install.source.fromConfig")}</span>}
              </legend>
              <SourceChoice label={t("install.source.label")} value={source} onChange={setSource} />
            </fieldset>
            <p className={styles.fieldNote}>
              {current.central
                ? t("install.source.holds", { version: current.central.version })
                : rich("install.source.holdsNone", {
                    link: <Link href={SERVER_UPDATE_PATH}>{t("install.source.importOne")}</Link>,
                  })}
            </p>
            {/* The URL is asked only for a custom source; one already set stays visible, as commands can choose it. */}
            {(source === "custom" || url !== "" || current.custom_url_from_config) && (
              <label className={styles.field}>
                <span>
                  {t("install.source.custom.label")}{" "}
                  <span className={styles.fieldNote}>
                    {current.custom_url_from_config ? t("install.source.custom.fromConfig") : t("install.source.custom.note")}
                  </span>
                </span>
                <input
                  className={fieldClass}
                  value={customUrl}
                  onChange={(e) => setCustomUrl(e.target.value)}
                  placeholder="https://files.example.com/monitorflow/"
                  disabled={!editable || current.custom_url_from_config}
                  aria-invalid={!urlValid}
                  spellCheck={false}
                  autoCapitalize="none"
                />
              </label>
            )}
            {editable && (
              <div className={styles.formFoot}>
                <Button
                  variant="k"
                  type="submit"
                  disabled={busy || !urlValid || (source === "custom" && url === "")}
                >
                  {t("install.save")}
                </Button>
                {saved && <span className={styles.footnote}>{t("install.saved")}</span>}
              </div>
            )}
          </>
        )}
        {error && (
          <p className={styles.formError} role="alert">
            {error}
          </p>
        )}
      </div>
    </form>
  );
}

function InstallCommand({ name, note, text }: { name: string; note: string; text: string }) {
  const labelId = useId();
  return (
    <div className={styles.field}>
      <span id={labelId}>
        {name} <span className={styles.fieldNote}>({note})</span>
      </span>
      <div className={styles.snippet}>
        <pre className={styles.code} aria-labelledby={labelId}>
          {text}
        </pre>
        <CopyButton text={text} name={t("install.command.copy", { name })} size="small" className={styles.snippetCopy} />
      </div>
    </div>
  );
}

function TokenTable({
  tokens,
  more,
  now,
  onChange,
}: {
  tokens: EnrollmentToken[] | undefined;
  more: boolean;
  now: Date;
  onChange: () => void;
}) {
  const titleId = useId();
  const [hideDone, setHideDone] = useState(true);
  const pending = tokens?.filter((token) => token.state === "pending").length ?? 0;
  const rows = (hideDone ? tokens?.filter((token) => token.state === "pending") : tokens) ?? [];

  const columns: Column<EnrollmentToken>[] = [
    {
      key: "peer",
      header: t("install.tokens.column.name"),
      cell: (token) => <span className={token.state === "pending" ? styles.peer : styles.mono}>{token.peer_id}</span>,
    },
    { key: "role", header: t("install.tokens.column.role"), cell: (token) => roleWord(token.role) },
    { key: "site", header: t("install.tokens.column.site"), cell: (token) => token.site_name },
    { key: "state", header: t("install.tokens.column.state"), cell: (token) => <StatusLabel kind="token" value={token.state} /> },
    {
      key: "created",
      header: t("install.tokens.column.created"),
      cell: (token) => (
        <>
          {formatWhen(new Date(token.created_at), now)}{" "}
          <span className={styles.dim}>
            {t("install.tokens.by", { name: token.created_by_username ?? t("install.tokens.byUnknown") })}
          </span>
        </>
      ),
    },
    {
      key: "expires",
      header: t("install.tokens.column.expires"),
      cell: (token) => {
        if (token.state === "used" && token.used_at) return t("install.tokens.used", { when: formatWhen(new Date(token.used_at), now) });
        const expires = new Date(token.expires_at);
        return token.state === "pending" ? (
          <>
            {formatWhen(expires, now)} <span className={styles.dim}>· {formatAgo(expires, now)}</span>
          </>
        ) : (
          formatWhen(expires, now)
        );
      },
    },
  ];

  return (
    <section className={styles.section} aria-labelledby={titleId}>
      <div className={styles.sectionHead}>
        <h2 id={titleId} className={styles.sectionTitle}>
          {t("install.tokens.title")}
        </h2>
        <span className={styles.dim}>{t("install.tokens.pending", { count: pending })}</span>
        <Switch rowClassName={styles.check} checked={hideDone} onChange={setHideDone} label={t("install.tokens.hideDone")} />
      </div>
      {tokens && rows.length === 0 ? (
        <p className={styles.sectionNote}>{tokens.length === 0 ? t("install.tokens.none") : t("install.tokens.nonePending")}</p>
      ) : (
        <DataTable
          bare
          label={t("install.tokens.label")}
          columns={columns}
          rows={rows}
          rowKey={(token) => token.id}
          rowMuted={(token) => token.state !== "pending"}
          loading={!tokens}
          rowActions={(token) =>
            token.state === "pending" ? (
              <InlineConfirm
                layout="row"
                trigger={t("install.tokens.revoke")}
                triggerVariant="default"
                triggerName={t("install.tokens.revokeName", { name: token.peer_id })}
                title={t("install.tokens.revokeTitle")}
                consequence={t("install.tokens.revokeConsequence", { name: token.peer_id })}
                confirmLabel={t("install.tokens.revokeConfirm")}
                pendingLabel={t("install.revoking")}
                onConfirm={async () => {
                  await api.revokeToken(token.id);
                  onChange();
                }}
                describeError={(e) => t("install.tokens.revokeFailed", { error: describeError(e) })}
              />
            ) : null
          }
        />
      )}
      {more && <p className={styles.sectionNote}>{t("install.tokens.newest", { count: tokens?.length ?? 0 })}</p>}
    </section>
  );
}
