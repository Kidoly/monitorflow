"use client";

import { type FormEvent, useCallback, useState } from "react";

import {
  type CreatedToken,
  type EnrollmentToken,
  type Me,
  type Site,
  type TokenRole,
  api,
  describeError,
  isAbortError,
} from "@/lib/api";
import { formatDateTime } from "@/lib/format";
import { administersCustomer } from "@/lib/me";
import { usePolling } from "@/lib/usePolling";

import styles from "./Admin.module.css";
import { AdminLayout } from "./AdminNav";
import { REFRESH_MS } from "./FleetOverview";

/** Validity choices; the API takes 60 s to 7 days. */
const TTLS: [number, string][] = [
  [900, "15 minutes"],
  [3600, "1 hour"],
  [86_400, "1 day"],
  [604_800, "7 days"],
];

/** `/admin/tokens/`: create, list and revoke enrollment tokens (ADR-0029). */
export function TokensAdminPage() {
  return <AdminLayout>{(me) => <TokensAdmin me={me} />}</AdminLayout>;
}

export function TokensAdmin({ me }: { me: Me }) {
  const [tokens, setTokens] = useState<EnrollmentToken[] | null>(null);
  const [more, setMore] = useState(false);
  const [sites, setSites] = useState<Site[]>([]);
  const [error, setError] = useState<string | null>(null);
  // Held in memory only, until the page is left: the API never returns it again.
  const [created, setCreated] = useState<CreatedToken | null>(null);

  const load = useCallback(async (signal?: AbortSignal) => {
    try {
      const [list, siteList] = await Promise.all([api.tokens(signal), api.sites(signal)]);
      setTokens(list.tokens);
      setMore(list.next_cursor !== null);
      setSites(siteList.sites);
      setError(null);
    } catch (e) {
      if (!isAbortError(e)) setError(describeError(e));
    }
  }, []);
  usePolling(load, REFRESH_MS, administersCustomer(me));

  if (!administersCustomer(me)) {
    return (
      <p className={styles.empty}>
        Enrollment tokens need an administrator role on a whole customer; a site administrator role is not enough.
      </p>
    );
  }
  const administered = sites.filter((site) => administersCustomer(me, site.customer_id));
  return (
    <section aria-labelledby="tokens-title">
      <h1 id="tokens-title">Enrollment tokens</h1>
      {error && (
        <p className={styles.error} role="alert">
          {error}
        </p>
      )}
      {created ? (
        <NewToken token={created} onDone={() => setCreated(null)} />
      ) : (
        <CreateToken
          sites={administered}
          onCreated={(token) => {
            setCreated(token);
            void load();
          }}
        />
      )}
      <h2>Tokens</h2>
      {!tokens && !error && <p role="status">Loading tokens…</p>}
      {tokens?.length === 0 && <p className={styles.empty}>No tokens yet.</p>}
      {tokens && tokens.length > 0 && <TokenTable tokens={tokens} onChange={() => void load()} />}
      {more && <p className={styles.note}>Showing the newest {tokens?.length} tokens.</p>}
    </section>
  );
}

function CreateToken({ sites, onCreated }: { sites: Site[]; onCreated: (token: CreatedToken) => void }) {
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
      const serverNames = role === "relay" ? names.split(/[\s,]+/).filter(Boolean) : [];
      const token = await api.createToken({
        role,
        peer_id: peerId.trim(),
        site_id: siteId || (sites[0]?.id ?? ""),
        ttl_s: ttl,
        server_names: serverNames,
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

  if (sites.length === 0) {
    return <p className={styles.note}>No site to enroll into yet. Create one under Sites first.</p>;
  }
  return (
    <form className={styles.form} onSubmit={submit} aria-label="New enrollment token">
      <label>
        Role
        <select value={role} onChange={(e) => setRole(e.target.value as TokenRole)}>
          <option value="agent">Agent</option>
          <option value="relay">Relay</option>
        </select>
      </label>
      <label>
        Peer ID
        <input
          value={peerId}
          onChange={(e) => setPeerId(e.target.value)}
          required
          pattern="[A-Za-z0-9._\-]{1,64}"
          title="1 to 64 letters, digits, dots, dashes or underscores"
        />
      </label>
      <label>
        Site
        <select value={siteId || sites[0]?.id} onChange={(e) => setSiteId(e.target.value)}>
          {sites.map((site) => (
            <option key={site.id} value={site.id}>
              {site.customer_name} / {site.name}
            </option>
          ))}
        </select>
      </label>
      <label>
        Valid for
        <select value={ttl} onChange={(e) => setTtl(Number(e.target.value))}>
          {TTLS.map(([seconds, label]) => (
            <option key={seconds} value={seconds}>
              {label}
            </option>
          ))}
        </select>
      </label>
      {role === "relay" && (
        <label>
          Server names (agents connect to these)
          <textarea
            value={names}
            onChange={(e) => setNames(e.target.value)}
            required
            rows={2}
            placeholder="relay1.paris.example 10.0.0.5"
          />
        </label>
      )}
      <button className={styles.button} type="submit" disabled={busy}>
        Create token
      </button>
      {error && (
        <p className={styles.error} role="alert">
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

function NewToken({ token, onDone }: { token: CreatedToken; onDone: () => void }) {
  const [copied, setCopied] = useState<string | null>(null);
  const snippet = configSnippet(token);
  const file = token.role === "relay" ? "/etc/monitorflow/relay.toml" : "/etc/monitorflow/agent.toml";

  async function copy(text: string, what: string) {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(`${what} copied.`);
    } catch {
      setCopied("Copying failed; select the text instead.");
    }
  }

  return (
    <section className={styles.success} aria-labelledby="new-token-title">
      <h2 id="new-token-title">
        Token for {token.role} {token.peer_id} on {token.site_name}
      </h2>
      <p>
        <strong>Copy it now: it is shown only once.</strong> It is valid until {formatDateTime(token.expires_at)} and
        works once.
      </p>
      <p>
        <code aria-label="Enrollment token">{token.token}</code>{" "}
        <button className={styles.secondary} type="button" onClick={() => void copy(token.token, "Token")}>
          Copy token
        </button>
      </p>
      <p>
        Add to <code>{file}</code>, or pass the token in <code>MONITORFLOW_ENROLLMENT_TOKEN</code> instead:
      </p>
      <pre aria-label="Configuration">{snippet}</pre>
      {token.role === "agent" && (
        <p className={styles.note}>
          For an agent behind a relay, set <code>upstream_url</code> to the relay&apos;s address.
        </p>
      )}
      <p>
        <button className={styles.secondary} type="button" onClick={() => void copy(snippet, "Configuration")}>
          Copy configuration
        </button>{" "}
        <button className={styles.button} type="button" onClick={onDone}>
          Done
        </button>
      </p>
      {copied && <p role="status">{copied}</p>}
    </section>
  );
}

function TokenTable({ tokens, onChange }: { tokens: EnrollmentToken[]; onChange: () => void }) {
  const [confirming, setConfirming] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function revoke(id: string) {
    setError(null);
    try {
      await api.revokeToken(id);
      setConfirming(null);
      onChange();
    } catch (e) {
      setError(`Revoking the token failed. ${describeError(e)}`);
    }
  }

  return (
    <>
      {error && (
        <p className={styles.error} role="alert">
          {error}
        </p>
      )}
      <div className={styles.scroll}>
        <table className={styles.table} aria-label="Enrollment tokens">
          <thead>
            <tr>
              <th scope="col">Peer</th>
              <th scope="col">Role</th>
              <th scope="col">Site</th>
              <th scope="col">State</th>
              <th scope="col">Created</th>
              <th scope="col">Expires</th>
              <th scope="col">Actions</th>
            </tr>
          </thead>
          <tbody>
            {tokens.map((token) => (
              <tr key={token.id}>
                <th scope="row">{token.peer_id}</th>
                <td>
                  {token.role}
                  {token.server_names.length > 0 && ` (${token.server_names.join(", ")})`}
                </td>
                <td>
                  {token.customer_name} / {token.site_name}
                </td>
                <td>{token.state === "used" && token.used_at ? `used ${formatDateTime(token.used_at)}` : token.state}</td>
                <td>
                  {formatDateTime(token.created_at)} by {token.created_by_username ?? "the server CLI"}
                </td>
                <td>{formatDateTime(token.expires_at)}</td>
                <td>
                  {token.state === "pending" &&
                    (confirming === token.id ? (
                      <>
                        <button className={styles.danger} type="button" onClick={() => void revoke(token.id)}>
                          Confirm revoke
                        </button>{" "}
                        <button className={styles.secondary} type="button" onClick={() => setConfirming(null)}>
                          Cancel
                        </button>
                      </>
                    ) : (
                      <button
                        className={styles.secondary}
                        type="button"
                        aria-label={`Revoke the token for ${token.peer_id}`}
                        onClick={() => setConfirming(token.id)}
                      >
                        Revoke
                      </button>
                    ))}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </>
  );
}
