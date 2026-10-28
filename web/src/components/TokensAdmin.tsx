"use client";

import { type FormEvent, type RefObject, useEffect, useId, useRef, useState } from "react";

import { Button, fieldClass } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";
import { type Column, DataTable } from "@/components/ui/DataTable";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { Banner, EmptyState, ErrorState, NoPermission, PageHeader, StaleBanner } from "@/components/ui/PageState";
import { CopyButton, SecretField } from "@/components/ui/SecretField";
import { Label, StatusLabel } from "@/components/ui/StatusLabel";
import { formatAgo } from "@/components/ui/time";
import {
  type AgentInstall,
  type CreatedToken,
  type EnrollmentToken,
  type InstallSource,
  type Me,
  type Site,
  type TokenRole,
  api,
  describeError,
} from "@/lib/api";
import { formatWhen } from "@/lib/clock";
import {
  type Command,
  IMPORT_COMMAND,
  SOURCE_WORDS,
  isSafeUrl,
  isServerAddress,
  isMirrorUrl,
  linuxInstallCommand,
  relayInstallCommand,
  windowsInstallCommand,
} from "@/lib/install";
import { administersCustomer, administersEverything } from "@/lib/me";
import { usePolledData } from "@/lib/usePolledData";

import { AdminLayout } from "./AdminLayout";
import styles from "./AdminPages.module.css";

/** Validity choices; the API takes 60 s to 7 days. */
const TTLS: [number, string][] = [
  [900, "15 minutes"],
  [3600, "1 hour"],
  [86_400, "1 day"],
  [604_800, "7 days"],
];

const ROLE_WORDS: Record<TokenRole, string> = { agent: "Agent", relay: "Relay" };

/** `/admin/tokens/`: create, list and revoke enrollment tokens (ADR-0029). */
export function TokensAdminPage() {
  return <AdminLayout>{(me) => <TokensAdmin me={me} />}</AdminLayout>;
}

/** `?role=relay` (the relays page's "New relay token") preselects Relay. */
function initialRole(): TokenRole {
  if (typeof window === "undefined") return "agent";
  return new URLSearchParams(window.location.search).get("role") === "relay" ? "relay" : "agent";
}

async function load(signal: AbortSignal) {
  const [list, { sites }] = await Promise.all([api.tokens(signal), api.sites(signal)]);
  return { tokens: list.tokens, more: list.next_cursor !== null, sites };
}

export function TokensAdmin({ me }: { me: Me }) {
  const allowed = administersCustomer(me);
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(load, { enabled: allowed });
  // Held in memory only, until dismissed or the page is left: the API never returns it again.
  const [created, setCreated] = useState<CreatedToken | null>(null);
  const peerRef = useRef<HTMLInputElement>(null);

  if (!allowed) {
    return (
      <NoPermission title="Enrollment tokens need a customer administrator role.">
        An administrator role on a whole customer is needed; a site administrator role is not enough.
      </NoPermission>
    );
  }
  const administered = data?.sites.filter((site) => administersCustomer(me, site.customer_id)) ?? [];
  return (
    <div className={styles.page}>
      <PageHeader
        title="Enrollment tokens"
        meta="A token lets one agent or relay enroll once. It is shown a single time, at creation."
      />
      {stale && updatedAt && (
        <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />
      )}
      {!data && error ? (
        <ErrorState what="enrollment tokens" error={describeError(error)} onRetry={refresh} />
      ) : (
        <>
          {data && administered.length === 0 ? (
            <EmptyState title="No site to enroll into yet">Create one under Sites first.</EmptyState>
          ) : (
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
              onDone={() => setCreated(null)}
              onAnother={() => {
                setCreated(null);
                peerRef.current?.focus();
              }}
            />
          )}
          <TokenTable tokens={data?.tokens} more={data?.more ?? false} now={updatedAt ?? new Date()} onChange={refresh} />
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
  const [role, setRole] = useState<TokenRole>(initialRole);
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
      setError(`Creating the token failed. ${describeError(e)}`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className={styles.panel} onSubmit={submit} aria-labelledby={titleId}>
      <h2 id={titleId} className={styles.sectionTitle}>
        New token
      </h2>
      <div className={styles.formGrid}>
        <fieldset className={styles.fieldset}>
          <legend className={styles.legend}>Role</legend>
          <div className={styles.radios}>
            {(["agent", "relay"] as const).map((value) => (
              <label key={value}>
                <input type="radio" name="role" checked={role === value} onChange={() => setRole(value)} />{" "}
                {ROLE_WORDS[value]}
              </label>
            ))}
          </div>
        </fieldset>
        <label className={styles.field}>
          Peer ID
          <input
            ref={peerRef}
            className={cx(fieldClass, styles.mono)}
            value={peerId}
            onChange={(e) => setPeerId(e.target.value)}
            placeholder="hostname or ID"
            required
            pattern="[A-Za-z0-9._\-]{1,64}"
            title="1 to 64 letters, digits, dots, dashes or underscores"
            spellCheck={false}
            autoCapitalize="none"
          />
        </label>
        <label className={styles.field}>
          Site
          <select className={fieldClass} value={siteId || sites[0]?.id} onChange={(e) => setSiteId(e.target.value)}>
            {sites.map((site) => (
              <option key={site.id} value={site.id}>
                {site.customer_name} › {site.name}
              </option>
            ))}
          </select>
        </label>
        <label className={styles.field}>
          Expires in
          <select className={fieldClass} value={ttl} onChange={(e) => setTtl(Number(e.target.value))}>
            {TTLS.map(([seconds, label]) => (
              <option key={seconds} value={seconds}>
                {label}
              </option>
            ))}
          </select>
        </label>
      </div>
      <label className={styles.field}>
        <span>
          Server names{" "}
          <span className={styles.fieldNote}>— relays only; the DNS names or IPs agents use to reach it</span>
        </span>
        <input
          className={cx(fieldClass, styles.mono)}
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
          Create token
        </Button>
        <span className={styles.footnote}>Recorded in the audit log as {me.username}.</span>
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

function NewToken({ token, onDone, onAnother }: { token: CreatedToken; onDone: () => void; onAnother: () => void }) {
  const titleId = useId();
  const snippetId = useId();
  const snippet = configSnippet(token);
  const file = token.role === "relay" ? "/etc/monitorflow/relay.toml" : "/etc/monitorflow/agent.toml";
  return (
    <section className={cx(styles.panel, styles.created)} aria-labelledby={titleId} aria-live="polite">
      <div className={styles.panelTitle}>
        <Label tone="ok">Created</Label>
        <h2 id={titleId} className={styles.sectionTitle}>
          Token for {token.peer_id} · expires {formatWhen(new Date(token.expires_at), new Date())}
        </h2>
      </div>
      <Banner tone="warn" role="alert">
        <strong>This token will not be shown again.</strong> Copy it now and paste it into the {token.role}&apos;s
        configuration. If you lose it, revoke it and create a new one.
      </Banner>
      <div className={styles.createdGrid}>
        <div className={styles.stack}>
          <SecretField label="Token" value={token.token} copyName="Copy token" />
          <div className={styles.buttons}>
            <Button variant="k" onClick={onDone}>
              Done
            </Button>
            <Button onClick={onAnother}>Create another</Button>
          </div>
        </div>
        <div className={styles.field}>
          <span id={snippetId}>
            Configuration snippet <span className={styles.fieldNote}>— {file}</span>
          </span>
          <div className={styles.snippet}>
            <pre className={styles.code} aria-labelledby={snippetId}>
              {snippet}
            </pre>
            <CopyButton text={snippet} name="Copy configuration snippet" size="small" className={styles.snippetCopy} />
          </div>
          {token.role === "agent" && (
            <span className={styles.fieldNote}>
              Or pass the token in <code>MONITORFLOW_ENROLLMENT_TOKEN</code>. For an agent behind a relay, set{" "}
              <code>upstream_url</code> to the relay&apos;s address.
            </span>
          )}
        </div>
      </div>
      {token.role === "agent" ? <InstallCommands token={token} /> : <RelayInstallCommand token={token} />}
    </section>
  );
}

/** This page's origin, where the server's own release files are (ADR-0054). */
function pageOrigin(): string {
  return typeof window === "undefined" ? "" : window.location.origin;
}

function SourceRadios({
  name,
  value,
  onChange,
  disabled = false,
}: {
  name: string;
  value: InstallSource;
  onChange: (source: InstallSource) => void;
  disabled?: boolean;
}) {
  return (
    <div className={styles.radios}>
      {(Object.keys(SOURCE_WORDS) as InstallSource[]).map((source) => (
        <label key={source}>
          <input
            type="radio"
            name={name}
            checked={value === source}
            disabled={disabled}
            onChange={() => onChange(source)}
          />{" "}
          {SOURCE_WORDS[source]}
        </label>
      ))}
    </div>
  );
}

/** Ready-to-paste install commands for an agent token (ADR-0052), from a chosen source (ADR-0054). */
function InstallCommands({ token }: { token: CreatedToken }) {
  const titleId = useId();
  const sourceName = useId();
  const [source, setSource] = useState<InstallSource>(token.install.source);
  const [relay, setRelay] = useState("");
  const valid = relay.trim() === "" || isServerAddress(relay);
  const through = valid ? relay : "";
  const origin = pageOrigin();
  const commands: { name: string; note: string; command: Command }[] = [
    {
      name: "Linux",
      note: "as root, on a host with systemd",
      command: linuxInstallCommand(token, source, origin, through),
    },
    {
      name: "Windows",
      note: "in an elevated PowerShell",
      command: windowsInstallCommand(token, source, origin, through),
    },
  ];
  // One note when both commands are missing for the same reason.
  const reasons = commands.flatMap(({ command }) => ("unavailable" in command ? [command.unavailable] : []));
  const shared = reasons.length === 2 && reasons[0] === reasons[1] ? reasons[0] : null;
  const central = token.install.central;
  return (
    <div className={styles.stack} role="group" aria-labelledby={titleId}>
      <h3 id={titleId} className={styles.sectionTitle}>
        Install the agent
      </h3>
      <fieldset className={styles.fieldset}>
        <legend className={styles.legend}>Download from</legend>
        <SourceRadios name={sourceName} value={source} onChange={setSource} />
      </fieldset>
      {source === "central" && central && !shared && (
        <p className={styles.fieldNote}>
          Release {central.version} from this server. The commands check its checksums, so hosts need not trust this
          site&apos;s certificate.
        </p>
      )}
      <label className={styles.field}>
        <span>
          Connect through a relay <span className={styles.fieldNote}>— optional; host:port agents use to reach it</span>
        </span>
        <input
          className={cx(fieldClass, styles.mono)}
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
          Enter a host name or IP address, with an optional port; IPv6 addresses in brackets.
        </p>
      )}
      {shared ? (
        <Banner tone="warn">{shared}</Banner>
      ) : (
        commands.map(({ name, note, command }) =>
          "text" in command ? (
            <InstallCommand key={name} name={name} note={note} text={command.text} />
          ) : (
            <Banner key={name} tone="warn">
              {name}: {command.unavailable}
            </Banner>
          ),
        )
      )}
    </div>
  );
}

/**
 * The relay's ready-to-paste install command (ADR-0055), from the chosen
 * source like the agent's, with the optional mirror URL.
 */
function RelayInstallCommand({ token }: { token: CreatedToken }) {
  const titleId = useId();
  const sourceName = useId();
  const [source, setSource] = useState<InstallSource>(token.install.source);
  const [mirror, setMirror] = useState("");
  const valid = mirror.trim() === "" || isMirrorUrl(mirror);
  const command = relayInstallCommand(token, source, pageOrigin(), valid ? mirror : "");
  const central = token.install.central;
  return (
    <div className={styles.stack} role="group" aria-labelledby={titleId}>
      <h3 id={titleId} className={styles.sectionTitle}>
        Install the relay
      </h3>
      <fieldset className={styles.fieldset}>
        <legend className={styles.legend}>Download from</legend>
        <SourceRadios name={sourceName} value={source} onChange={setSource} />
      </fieldset>
      {source === "central" && central && "text" in command && (
        <p className={styles.fieldNote}>
          Release {central.version} from this server. The command checks its checksums, so the host need not trust this
          site&apos;s certificate.
        </p>
      )}
      <label className={styles.field}>
        <span>
          Serve mirrors at{" "}
          <span className={styles.fieldNote}>
            — optional; the URL this site&apos;s hosts reach the relay&apos;s mirror listener at
          </span>
        </span>
        <input
          className={cx(fieldClass, styles.mono)}
          value={mirror}
          onChange={(e) => setMirror(e.target.value)}
          placeholder="http://relay1.paris.example:7480"
          aria-invalid={!valid}
          spellCheck={false}
          autoCapitalize="none"
        />
      </label>
      {!valid && (
        <p className={styles.formError} role="alert">
          Enter an http or https URL, like http://relay1.paris.example:7480.
        </p>
      )}
      {"text" in command ? (
        <InstallCommand name="Linux" note="as root, on a host with systemd" text={command.text} />
      ) : (
        <Banner tone="warn">{command.unavailable}</Banner>
      )}
      <p className={styles.fieldNote}>
        Then install the site&apos;s agents with a token each, giving this relay&apos;s address in &quot;Connect through
        a relay&quot;. The relay updates itself with the agents (Updates › Automatic updates).
      </p>
    </div>
  );
}

/** The server-wide default source (ADR-0054); global administrators change it. */
function DefaultInstallSource({ editable }: { editable: boolean }) {
  const titleId = useId();
  const sourceName = useId();
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
        if (!controller.signal.aborted) setError(`Loading the install source failed. ${describeError(e)}`);
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
      setError(`Saving the install source failed. ${describeError(e)}`);
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
          Default install source
        </h2>
      </div>
      <p className={styles.sectionNote}>
        Where the install commands of new tokens download the agent or relay from; each command can use another.
        {!editable && " A global administrator changes it."}
      </p>
      {current && (
        <>
          <fieldset className={styles.fieldset} disabled={!editable || current.source_from_config}>
            <legend className={styles.legend}>
              Source{" "}
              {current.source_from_config && (
                <span className={styles.fieldNote}>— set in the server configuration (agent_install_source)</span>
              )}
            </legend>
            <SourceRadios name={sourceName} value={source} onChange={setSource} />
          </fieldset>
          <p className={styles.fieldNote}>
            {current.central
              ? `This server holds release ${current.central.version}.`
              : `This server holds no agent release yet: import one with ${IMPORT_COMMAND}.`}
          </p>
          <label className={styles.field}>
            <span>
              Custom URL{" "}
              <span className={styles.fieldNote}>
                {current.custom_url_from_config
                  ? "— set in the server configuration (agent_download_url)"
                  : "— https directory holding the release files"}
              </span>
            </span>
            <input
              className={cx(fieldClass, styles.mono)}
              value={customUrl}
              onChange={(e) => setCustomUrl(e.target.value)}
              placeholder="https://files.example.com/monitorflow/0.3.0"
              disabled={!editable || current.custom_url_from_config}
              aria-invalid={!urlValid}
              spellCheck={false}
              autoCapitalize="none"
            />
          </label>
          {editable && (
            <div className={styles.formFoot}>
              <Button
                variant="k"
                type="submit"
                disabled={busy || !urlValid || (source === "custom" && url === "")}
              >
                Save
              </Button>
              {saved && <span className={styles.footnote}>Saved.</span>}
            </div>
          )}
        </>
      )}
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
    </form>
  );
}

function InstallCommand({ name, note, text }: { name: string; note: string; text: string }) {
  const labelId = useId();
  return (
    <div className={styles.field}>
      <span id={labelId}>
        {name} <span className={styles.fieldNote}>— {note}</span>
      </span>
      <div className={styles.snippet}>
        <pre className={styles.code} aria-labelledby={labelId}>
          {text}
        </pre>
        <CopyButton text={text} name={`Copy the ${name} command`} size="small" className={styles.snippetCopy} />
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
      header: "Peer",
      cell: (token) => <span className={token.state === "pending" ? styles.peer : styles.mono}>{token.peer_id}</span>,
    },
    { key: "role", header: "Role", cell: (token) => ROLE_WORDS[token.role] },
    { key: "site", header: "Site", cell: (token) => token.site_name },
    { key: "state", header: "State", cell: (token) => <StatusLabel kind="token" value={token.state} /> },
    {
      key: "created",
      header: "Created",
      cell: (token) => (
        <>
          {formatWhen(new Date(token.created_at), now)}{" "}
          <span className={styles.dim}>by {token.created_by_username ?? "the server CLI"}</span>
        </>
      ),
    },
    {
      key: "expires",
      header: "Expires",
      cell: (token) => {
        if (token.state === "used" && token.used_at) return `used ${formatWhen(new Date(token.used_at), now)}`;
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
          Tokens
        </h2>
        <span className={styles.dim}>{pending} pending</span>
        <label className={styles.check}>
          <input type="checkbox" checked={hideDone} onChange={(e) => setHideDone(e.target.checked)} /> Hide used and
          expired
        </label>
      </div>
      {tokens && rows.length === 0 ? (
        <p className={styles.sectionNote}>{tokens.length === 0 ? "No tokens yet." : "No pending tokens."}</p>
      ) : (
        <DataTable
          bare
          label="Enrollment tokens"
          columns={columns}
          rows={rows}
          rowKey={(token) => token.id}
          rowMuted={(token) => token.state !== "pending"}
          loading={!tokens}
          rowActions={(token) =>
            token.state === "pending" ? (
              <InlineConfirm
                layout="row"
                trigger="Revoke"
                triggerVariant="default"
                triggerName={`Revoke the token for ${token.peer_id}`}
                title="Revoke this token?"
                consequence={`${token.peer_id} will not be able to enroll with it.`}
                confirmLabel="Revoke token"
                pendingLabel="Revoking…"
                onConfirm={async () => {
                  await api.revokeToken(token.id);
                  onChange();
                }}
                describeError={(e) => `Revoking the token failed. ${describeError(e)}`}
              />
            ) : (
              <span className={styles.dim}>—</span>
            )
          }
        />
      )}
      {more && <p className={styles.sectionNote}>Showing the newest {tokens?.length} tokens.</p>}
    </section>
  );
}
