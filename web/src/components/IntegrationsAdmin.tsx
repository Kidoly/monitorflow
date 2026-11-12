"use client";

import {
  type FormEvent,
  type ReactNode,
  useCallback,
  useId,
  useState,
} from "react";

import {
  ApiError,
  type Customer,
  type IntegrationDelivery,
  type IntegrationEndpoint,
  type IntegrationTestResult,
  type IntegrationWithSecret,
  type Site,
  api,
  describeError,
} from "@/lib/api";
import {
  EVENT_GROUPS,
  INTEGRATIONS_GUIDE,
  deliveryResult,
  eventName,
  eventsSummary,
  localToIso,
  stateLabel,
} from "@/lib/integrations";
import {
  administersEverything,
  isAdministrator,
  multiCustomer,
  useMe,
} from "@/lib/me";
import {
  type ChannelScope,
  channelScopes,
  scopeName,
  scopeValue,
} from "@/lib/notifications";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./AdminPages.module.css";
import own from "./IntegrationsAdmin.module.css";
import { Button, fieldClass } from "./ui/Button";
import { type Column, DataTable } from "./ui/DataTable";
import { InlineConfirm } from "./ui/InlineConfirm";
import {
  Banner,
  EmptyState,
  ErrorState,
  LoadingBlock,
  NoPermission,
  PageHeader,
} from "./ui/PageState";
import { RelativeTime } from "./ui/RelativeTime";
import { SecretField } from "./ui/SecretField";
import { Label } from "./ui/StatusLabel";

/** `/admin/integrations/`: administrators, each of what they administer, like the API (ADR-0102). */
export function IntegrationsPage() {
  const me = useMe();
  if (!isAdministrator(me)) {
    return (
      <NoPermission title="Integrations need an administrator">
        Ask one to send MonitorFlow&apos;s events to your other tools.
      </NoPermission>
    );
  }
  return <Integrations />;
}

/**
 * What an integration's coverage means: the notification notes speak of
 * alerts and the central server. Customers are named only to someone who
 * sees several (ADR-0104); otherwise the customer as a whole is all sites.
 */
function integrationNote(scope: ChannelScope, several: boolean): string {
  if (scope.site_id) return `Events of ${scope.label.split(" / ").pop()} only.`;
  if (!several) {
    return scope.customer_id
      ? "Events of all your sites."
      : "Events of every site.";
  }
  if (scope.customer_id)
    return `Events of ${scope.label.replace(/^All of /, "")} and all its sites. Nothing from other customers.`;
  return "Events of every customer and site.";
}

type Editing = { endpoint?: IntegrationEndpoint };

function Integrations() {
  const { data, error, refresh } = usePolledData(api.integrations);
  const customers = usePolledData(api.customers, { intervalMs: 60_000 });
  const sites = usePolledData(api.sites, { intervalMs: 60_000 });
  const me = useMe();
  const [editing, setEditing] = useState<Editing | null>(null);
  const [secret, setSecret] = useState<IntegrationWithSecret | null>(null);
  const [tests, setTests] = useState<
    Record<string, IntegrationTestResult | "sending">
  >({});
  const [log, setLog] = useState<IntegrationEndpoint | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  async function test(id: string) {
    setTests((t) => ({ ...t, [id]: "sending" }));
    try {
      const result = await api.testIntegration(id);
      setTests((t) => ({ ...t, [id]: result }));
    } catch (e) {
      setTests((t) => ({
        ...t,
        [id]: { ok: false, status: null, error: describeError(e) },
      }));
    }
  }

  async function act(action: () => Promise<unknown>) {
    setActionError(null);
    try {
      await action();
    } catch (e) {
      setActionError(describeError(e));
    }
    refresh();
  }

  const header = (
    <PageHeader
      title="Integrations"
      meta="Signed events for your other tools"
    />
  );
  if (error instanceof ApiError && error.status === 403) {
    return (
      <div className={styles.page}>
        {header}
        <NoPermission title="Integrations need an administrator" />
      </div>
    );
  }
  if (!data) {
    return (
      <div className={styles.page}>
        {header}
        {error ? (
          <ErrorState
            what="integrations"
            error={describeError(error)}
            onRetry={refresh}
          />
        ) : (
          <LoadingBlock />
        )}
      </div>
    );
  }

  const allCustomers = customers.data?.customers ?? [];
  const allSites = sites.data?.sites ?? [];
  const global = administersEverything(me);
  const add = (
    <Button variant="k" onClick={() => setEditing({})}>
      Add integration
    </Button>
  );
  return (
    <div className={styles.page}>
      {header}
      {actionError && <Banner tone="bad">{actionError}</Banner>}
      <p className={styles.footnote}>
        MonitorFlow posts a signed JSON request to each integration when
        incidents, servers, updates, campaigns or approvals change, for a PSA or
        your own automation. Each event comes at least once, so receivers keep
        its ID to ignore repeats.{" "}
        <a href={INTEGRATIONS_GUIDE}>How to receive them</a>. To tell people,
        use Notifications.
      </p>
      {secret && (
        <section className={styles.panel} aria-label="Signing secret">
          <Banner tone="warn">
            Copy the signing secret of {secret.endpoint.name} into the receiver
            now: it is not shown again.
          </Banner>
          <SecretField
            label="Signing secret"
            value={secret.secret}
            copyName="Copy the signing secret"
          />
          <div className={styles.formFoot}>
            <Button onClick={() => void test(secret.endpoint.id)}>
              Send test event
            </Button>
            <Button onClick={() => setSecret(null)}>Done</Button>
            <TestStatus test={tests[secret.endpoint.id]} />
          </div>
        </section>
      )}
      {editing ? (
        <IntegrationForm
          key={editing.endpoint?.id ?? "new"}
          endpoint={editing.endpoint}
          events={data.events}
          customers={allCustomers}
          sites={allSites}
          onCancel={() => setEditing(null)}
          onSaved={(saved) => {
            setEditing(null);
            if (saved.secret) setSecret(saved.secret);
            refresh();
          }}
        />
      ) : data.endpoints.length === 0 ? (
        <EmptyState title="No integrations yet" actions={add}>
          Send incidents, servers, updates and approvals to a PSA such as
          HaloPSA, or to your own automation.
        </EmptyState>
      ) : (
        <div className={styles.buttonRow}>{add}</div>
      )}
      {data.endpoints.length > 0 && (
        <section className={styles.section} aria-label="Integrations">
          <Endpoints
            endpoints={data.endpoints}
            events={data.events}
            tests={tests}
            showScope={
              !global ||
              multiCustomer(me) ||
              allSites.length > 1 ||
              data.endpoints.some((e) => e.customer_id)
            }
            severalCustomers={multiCustomer(me)}
            onTest={(id) => void test(id)}
            onLog={setLog}
            onEdit={(endpoint) => setEditing({ endpoint })}
            onState={(id, state) =>
              act(() => api.setIntegrationState(id, state))
            }
            onNewSecret={async (id) =>
              setSecret(await api.newIntegrationSecret(id))
            }
            onDelete={async (id) => {
              await api.deleteIntegration(id);
              if (log?.id === id) setLog(null);
              refresh();
            }}
          />
        </section>
      )}
      {log && (
        <DeliveryLog key={log.id} endpoint={log} onClose={() => setLog(null)} />
      )}
    </div>
  );
}

function TestStatus({ test }: { test?: IntegrationTestResult | "sending" }) {
  if (!test) return null;
  if (test === "sending")
    return <span className={styles.dim}>Sending a test event…</span>;
  if (test.ok) {
    return (
      <span role="status">
        <Label tone="ok">Test accepted</Label>{" "}
        <span className={styles.dim}>The receiver answered {test.status}.</span>
      </span>
    );
  }
  return (
    <span role="alert">
      <Label tone="bad">Test failed</Label> {test.error}
    </span>
  );
}

function Status({
  endpoint,
  test,
}: {
  endpoint: IntegrationEndpoint;
  test?: IntegrationTestResult | "sending";
}) {
  if (test) return <TestStatus test={test} />;
  const now = new Date();
  const { text, tone } = stateLabel(endpoint);
  return (
    <>
      <Label tone={tone}>{text}</Label>{" "}
      {endpoint.state === "disabled" ? (
        <span>
          Nothing reached it for a day. Turn it on, then send again what it
          missed.
        </span>
      ) : endpoint.failing_since && endpoint.last_error ? (
        <span>{endpoint.last_error}</span>
      ) : endpoint.last_success_at ? (
        <span className={styles.dim}>
          last delivered{" "}
          <RelativeTime value={endpoint.last_success_at} now={now} />
        </span>
      ) : (
        <span className={styles.dim}>Nothing sent yet</span>
      )}
      {endpoint.pending > 0 && (
        <span className={styles.dim}> · {endpoint.pending} waiting</span>
      )}
    </>
  );
}

function host(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

function Endpoints({
  endpoints,
  events,
  tests,
  showScope,
  severalCustomers,
  onTest,
  onLog,
  onEdit,
  onState,
  onNewSecret,
  onDelete,
}: {
  endpoints: IntegrationEndpoint[];
  events: string[];
  tests: Record<string, IntegrationTestResult | "sending">;
  showScope: boolean;
  severalCustomers: boolean;
  onTest: (id: string) => void;
  onLog: (endpoint: IntegrationEndpoint) => void;
  onEdit: (endpoint: IntegrationEndpoint) => void;
  onState: (id: string, state: "on" | "paused") => Promise<void>;
  onNewSecret: (id: string) => Promise<void>;
  onDelete: (id: string) => Promise<void>;
}) {
  const columns: Column<IntegrationEndpoint>[] = [
    {
      key: "name",
      header: "Sends to",
      className: styles.wrap,
      cell: (e) => (
        <>
          <strong>{e.name}</strong>{" "}
          <span className={styles.dim}>{host(e.origin)}</span>
          <br />
          <span className={styles.dim}>{eventsSummary(e.events, events)}</span>
        </>
      ),
    },
    ...(showScope
      ? [
          {
            key: "scope",
            header: "About",
            cell: (e: IntegrationEndpoint) =>
              e.customer_id ? (
                scopeName(e, severalCustomers)
              ) : (
                <span className={styles.dim}>{scopeName(e, false)}</span>
              ),
          },
        ]
      : []),
    {
      key: "status",
      header: "Status",
      className: styles.wrap,
      cell: (e) => <Status endpoint={e} test={tests[e.id]} />,
    },
    {
      key: "actions",
      header: "",
      cell: (e) => (
        <span className={styles.buttons}>
          <Button
            size="small"
            disabled={tests[e.id] === "sending"}
            onClick={() => onTest(e.id)}
          >
            Send test event
          </Button>
          <Button size="small" onClick={() => onLog(e)}>
            Deliveries
          </Button>
          <Button size="small" onClick={() => onEdit(e)}>
            Edit
          </Button>
          {e.state === "on" ? (
            <Button size="small" onClick={() => void onState(e.id, "paused")}>
              Pause
            </Button>
          ) : (
            <Button size="small" onClick={() => void onState(e.id, "on")}>
              Turn on
            </Button>
          )}
          <InlineConfirm
            layout="row"
            trigger="New secret"
            triggerName={`New secret for ${e.name}`}
            title="Replace the signing secret?"
            consequence="The old one stops working at once: the receiver rejects requests until it has the new one."
            confirmLabel="Replace"
            onConfirm={() => onNewSecret(e.id)}
            describeError={describeError}
          />
          <InlineConfirm
            layout="row"
            trigger="Delete"
            triggerName={`Delete ${e.name}`}
            title="Delete this integration?"
            consequence="Nothing more is sent to it, including what is waiting."
            confirmLabel="Delete"
            onConfirm={() => onDelete(e.id)}
            describeError={describeError}
          />
        </span>
      ),
    },
  ];
  return (
    <DataTable
      bare
      label="Integrations"
      columns={columns}
      rows={endpoints}
      rowKey={(e) => e.id}
    />
  );
}

function IntegrationForm({
  endpoint,
  events: allEvents,
  customers,
  sites,
  onCancel,
  onSaved,
}: {
  endpoint?: IntegrationEndpoint;
  events: string[];
  customers: Customer[];
  sites: Site[];
  onCancel: () => void;
  onSaved: (saved: { secret?: IntegrationWithSecret }) => void;
}) {
  const me = useMe();
  const [name, setName] = useState(endpoint?.name ?? "");
  // The stored URL's path is never shown (ADR-0112): empty keeps it.
  const [url, setUrl] = useState("");
  const [events, setEvents] = useState<string[]>(endpoint?.events ?? allEvents);
  const [allowPrivate, setAllowPrivate] = useState(
    endpoint?.allow_private ?? false,
  );
  const [chosen, setChosen] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const id = useId();
  const global = administersEverything(me);
  const several = multiCustomer(me);
  const scopes = channelScopes(me, customers, sites, endpoint);
  const wanted = chosen ?? (endpoint ? scopeValue(endpoint) : scopes[0]?.value);
  const scope = scopes.find((s) => s.value === wanted);
  // Someone else keeps a global administrator's choice only for the same URL.
  const keepsPrivate =
    !global && !!endpoint?.allow_private && url.trim() === "";

  function toggle(event: string, on: boolean) {
    setEvents((current) =>
      on ? [...current, event] : current.filter((e) => e !== event),
    );
  }

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (!scope) return;
    if (events.length === 0) {
      setError("Choose at least one event.");
      return;
    }
    setBusy(true);
    setError(null);
    const body = {
      name,
      url,
      events: allEvents.filter((e) => events.includes(e)),
      customer_id: scope.customer_id,
      site_id: scope.site_id,
      allow_private: global ? allowPrivate : keepsPrivate,
    };
    try {
      if (endpoint) {
        await api.updateIntegration(endpoint.id, body);
        onSaved({});
      } else {
        onSaved({ secret: await api.createIntegration(body) });
      }
    } catch (e) {
      setError(
        e instanceof ApiError && e.status === 400
          ? e.message
          : describeError(e),
      );
      setBusy(false);
    }
  }

  const field = (label: string, input: ReactNode, note?: string) => (
    <label className={styles.field}>
      {label}
      {input}
      {note && <span className={styles.fieldNote}>{note}</span>}
    </label>
  );
  return (
    <form
      className={styles.panel}
      onSubmit={(e) => void submit(e)}
      aria-labelledby={`${id}-title`}
    >
      <h2 id={`${id}-title`} className={styles.sectionTitle}>
        {endpoint ? `Edit ${endpoint.name}` : "Add integration"}
      </h2>
      {field(
        "Name",
        <input
          className={fieldClass}
          required
          maxLength={80}
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="HaloPSA"
        />,
      )}
      {field(
        "URL",
        <input
          className={fieldClass}
          type="url"
          required={!endpoint}
          spellCheck={false}
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder={
            endpoint
              ? `${endpoint.origin}/…, saved; leave empty to keep it`
              : "https://psa.example.com/webhooks/monitorflow"
          }
        />,
        "Receives a signed JSON POST per event. Its path is not shown again: it often holds a token.",
      )}
      {scopes.length > 1
        ? field(
            "Send events about",
            <select
              className={fieldClass}
              value={wanted}
              onChange={(e) => setChosen(e.target.value)}
            >
              {scopes.map((s) => (
                <option key={s.value} value={s.value}>
                  {s.label}
                </option>
              ))}
            </select>,
            scope && integrationNote(scope, several),
          )
        : scope?.value !== "everything" &&
          scope && (
            <p className={styles.fieldNote}>
              {integrationNote(scope, several)}
            </p>
          )}
      <fieldset className={styles.fieldset}>
        <legend className={styles.legend}>Events</legend>
        <label>
          <input
            type="checkbox"
            checked={allEvents.every((e) => events.includes(e))}
            onChange={(e) => setEvents(e.target.checked ? allEvents : [])}
          />{" "}
          All events
        </label>
        <div className={own.events}>
          {EVENT_GROUPS.map((group) => (
            <fieldset key={group.title} className={styles.fieldset}>
              <legend className={styles.legend}>{group.title}</legend>
              {group.events
                .filter((e) => allEvents.includes(e.event))
                .map((e) => (
                  <label key={e.event}>
                    <input
                      type="checkbox"
                      checked={events.includes(e.event)}
                      onChange={(change) =>
                        toggle(e.event, change.target.checked)
                      }
                    />{" "}
                    {e.label}
                  </label>
                ))}
            </fieldset>
          ))}
        </div>
      </fieldset>
      {global ? (
        <label>
          <input
            type="checkbox"
            checked={allowPrivate}
            onChange={(e) => setAllowPrivate(e.target.checked)}
          />{" "}
          Allow private addresses, like 10.0.0.5 or a name on your network. Only
          for a receiver you trust: this server can then reach anything on its
          network.
        </label>
      ) : (
        keepsPrivate && (
          <p className={styles.fieldNote}>
            A global administrator allowed a private address for this URL.
            Changing the URL needs them again.
          </p>
        )
      )}
      <div className={styles.formFoot}>
        <Button type="submit" variant="k" disabled={busy || !scope}>
          {busy ? "Saving…" : "Save"}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          Cancel
        </Button>
        {error && (
          <p className={styles.formError} role="alert">
            {error}
          </p>
        )}
      </div>
    </form>
  );
}

/** The newest deliveries of `endpoint`, with Send again for one event or a time range. */
function DeliveryLog({
  endpoint,
  onClose,
}: {
  endpoint: IntegrationEndpoint;
  onClose: () => void;
}) {
  const fetcher = useCallback(
    (signal: AbortSignal) =>
      api.integrationDeliveries(endpoint.id, undefined, signal),
    [endpoint.id],
  );
  const { data, error, refresh } = usePolledData(fetcher);
  const [older, setOlder] = useState<IntegrationDelivery[]>([]);
  const [next, setNext] = useState<number | null | undefined>(undefined);
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [notice, setNotice] = useState<{
    tone: "ok" | "bad";
    text: string;
  } | null>(null);
  const id = useId();
  const now = new Date();
  const waits =
    endpoint.state === "on" ? "" : " It waits until the integration is on.";

  async function sendAgain(body: Parameters<typeof api.replayIntegration>[1]) {
    setNotice(null);
    try {
      const { queued } = await api.replayIntegration(endpoint.id, body);
      setNotice({
        tone: "ok",
        text: `${queued === 1 ? "1 event" : `${queued} events`} will be sent again, with the same IDs.${waits}`,
      });
      refresh();
    } catch (e) {
      setNotice({
        tone: "bad",
        text:
          e instanceof ApiError && e.status === 400
            ? e.message
            : describeError(e),
      });
    }
  }

  async function loadOlder() {
    const before = next ?? data?.next_before;
    if (before == null) return;
    const page = await api.integrationDeliveries(endpoint.id, before);
    setOlder((rows) => [...rows, ...page.deliveries]);
    setNext(page.next_before);
  }

  const rows = [...(data?.deliveries ?? []), ...older];
  const more = next === undefined ? data?.next_before : next;
  const columns: Column<IntegrationDelivery>[] = [
    {
      key: "when",
      header: "When",
      cell: (d) => <RelativeTime value={d.occurred_at} now={now} />,
    },
    {
      key: "event",
      header: "Event",
      cell: (d) => (
        <>
          {eventName(d.event)}
          {d.replay && <span className={styles.dim}> · sent again</span>}
        </>
      ),
    },
    {
      key: "result",
      header: "Result",
      className: styles.wrap,
      cell: (d) => (
        <>
          <Label
            tone={
              d.state === "delivered"
                ? "ok"
                : d.state === "failed"
                  ? "bad"
                  : "neutral"
            }
          >
            {deliveryResult(d)}
          </Label>
          {d.state !== "delivered" && d.last_error && <> {d.last_error}</>}
        </>
      ),
    },
    {
      key: "actions",
      header: "",
      cell: (d) => (
        <Button
          size="small"
          aria-label={`Send ${eventName(d.event)} again`}
          onClick={() => void sendAgain({ event_id: d.event_id })}
        >
          Send again
        </Button>
      ),
    },
  ];
  return (
    <section className={styles.section} aria-labelledby={`${id}-title`}>
      <div className={styles.sectionHead}>
        <h2 id={`${id}-title`} className={styles.sectionTitle}>
          Deliveries to {endpoint.name}
        </h2>
        <Button size="small" onClick={onClose}>
          Close
        </Button>
      </div>
      <div className={styles.sectionBody}>
        {notice && <Banner tone={notice.tone}>{notice.text}</Banner>}
        <form
          className={own.range}
          onSubmit={(e) => {
            e.preventDefault();
            const start = localToIso(from);
            if (!start) return;
            void sendAgain({
              from: start,
              ...(localToIso(to) ? { to: localToIso(to) ?? undefined } : {}),
            });
          }}
        >
          <label className={styles.field}>
            From
            <input
              className={fieldClass}
              type="datetime-local"
              required
              value={from}
              onChange={(e) => setFrom(e.target.value)}
            />
          </label>
          <label className={styles.field}>
            To
            <input
              className={fieldClass}
              type="datetime-local"
              value={to}
              onChange={(e) => setTo(e.target.value)}
            />
          </label>
          <Button type="submit">Send this time range again</Button>
        </form>
        <p className={styles.fieldNote}>
          To empty means now. Events are kept 30 days.
        </p>
      </div>
      {!data ? (
        error ? (
          <ErrorState
            what="deliveries"
            error={describeError(error)}
            onRetry={refresh}
          />
        ) : (
          <LoadingBlock />
        )
      ) : rows.length === 0 ? (
        <p className={styles.dim}>
          Nothing sent to it yet. Events it asks for show here as they happen.
        </p>
      ) : (
        <>
          <DataTable
            bare
            label={`Deliveries to ${endpoint.name}`}
            columns={columns}
            rows={rows}
            rowKey={(d) => String(d.id)}
          />
          {more != null && (
            <Button onClick={() => void loadOlder()}>Older deliveries</Button>
          )}
        </>
      )}
    </section>
  );
}
