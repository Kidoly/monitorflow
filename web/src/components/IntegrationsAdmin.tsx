"use client";

import {
  type FormEvent,
  type ReactNode,
  useCallback,
  useId,
  useState,
} from "react";

import { rich, t } from "@/i18n";
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
  eventLabel,
  eventName,
  eventsSummary,
  localToIso,
  groupTitle,
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
import { INTEGRATIONS_PATH } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";
import { Switch } from "@/components/ui/Switch";

import styles from "./AdminPages.module.css";
import own from "./IntegrationsAdmin.module.css";
import { Breadcrumb } from "./ui/Breadcrumb";
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

/** `/settings/integrations/webhooks/`: administrators, each of what they administer, like the API (ADR-0102). */
export function IntegrationsPage() {
  const me = useMe();
  if (!isAdministrator(me)) {
    return (
      <NoPermission title={t("integrations.needAdmin")}>{t("integrations.needAdminBody")}</NoPermission>
    );
  }
  return <Integrations />;
}

/**
 * What an integration's coverage means: the notification notes speak of
 * alerts and the central server. Customers are named only to someone who
 * sees several (ADR-0104); otherwise the customer as a whole is all sites.
 */
function integrationNote(scope: ChannelScope, several: boolean, customers: Customer[], sites: Site[]): string {
  if (scope.site_id) {
    const site = sites.find((s) => s.id === scope.site_id)?.name ?? scope.label.split(" / ").pop() ?? "";
    return t("integrations.note.site", { site });
  }
  if (!several) {
    return scope.customer_id ? t("integrations.note.allYourSites") : t("integrations.note.everySite");
  }
  if (scope.customer_id) {
    const customer = customers.find((c) => c.id === scope.customer_id)?.name ?? scope.label;
    return t("integrations.note.customer", { customer });
  }
  return t("integrations.note.everything");
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
    <>
      <Breadcrumb parents={[{ label: t("integrations.title"), href: INTEGRATIONS_PATH }]} />
      <PageHeader title={t("integrations.webhooksTitle")} />
    </>
  );
  if (error instanceof ApiError && error.status === 403) {
    return (
      <div className={styles.page}>
        {header}
        <NoPermission title={t("integrations.needAdmin")} />
      </div>
    );
  }
  if (!data) {
    return (
      <div className={styles.page}>
        {header}
        {error ? (
          <ErrorState
            what={t("integrations.what")}
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
      {t("integrations.add")}
    </Button>
  );
  return (
    <div className={styles.page}>
      {header}
      {actionError && <Banner tone="bad">{actionError}</Banner>}
      <p className={styles.footnote}>
        {rich("integrations.intro", {
          guide: <a href={INTEGRATIONS_GUIDE}>{t("integrations.guide")}</a>,
        })}
      </p>
      {secret && (
        <section className={styles.panel} aria-label={t("integrations.secret")}>
          <Banner tone="warn">{t("integrations.copySecretNow", { name: secret.endpoint.name })}</Banner>
          <SecretField
            label={t("integrations.secret")}
            value={secret.secret}
            copyName={t("integrations.copySecret")}
          />
          <div className={styles.formFoot}>
            <Button onClick={() => void test(secret.endpoint.id)}>{t("integrations.sendTest")}</Button>
            <Button onClick={() => setSecret(null)}>{t("integrations.done")}</Button>
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
        <EmptyState title={t("integrations.emptyTitle")} actions={add} />
      ) : (
        <div className={styles.buttonRow}>{add}</div>
      )}
      {data.endpoints.length > 0 && (
        <section className={styles.section} aria-label={t("integrations.title")}>
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
    return <span className={styles.dim}>{t("integrations.sendingTest")}</span>;
  if (test.ok) {
    return (
      <span role="status">
        <Label tone="ok">{t("integrations.testAccepted")}</Label>{" "}
        <span className={styles.dim}>{t("integrations.receiverAnswered", { status: String(test.status) })}</span>
      </span>
    );
  }
  return (
    <span role="alert">
      <Label tone="bad">{t("integrations.testFailed")}</Label> {test.error}
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
        <span>{t("integrations.disabledNote")}</span>
      ) : endpoint.failing_since && endpoint.last_error ? (
        <span>{endpoint.last_error}</span>
      ) : endpoint.last_success_at ? (
        <span className={styles.dim}>
          {rich("integrations.lastDelivered", {
            when: <RelativeTime value={endpoint.last_success_at} now={now} />,
          })}
        </span>
      ) : (
        <span className={styles.dim}>{t("integrations.nothingSent")}</span>
      )}
      {endpoint.pending > 0 && (
        <span className={styles.dim}> · {t("integrations.waiting", { count: endpoint.pending })}</span>
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
      header: t("integrations.column.sendsTo"),
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
            header: t("integrations.column.about"),
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
      header: t("integrations.column.status"),
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
            {t("integrations.sendTest")}
          </Button>
          <Button size="small" onClick={() => onLog(e)}>
            {t("integrations.deliveries")}
          </Button>
          <Button size="small" onClick={() => onEdit(e)}>
            {t("integrations.edit")}
          </Button>
          {e.state === "on" ? (
            <Button size="small" onClick={() => void onState(e.id, "paused")}>
              {t("integrations.pause")}
            </Button>
          ) : (
            <Button size="small" onClick={() => void onState(e.id, "on")}>
              {t("integrations.turnOn")}
            </Button>
          )}
          <InlineConfirm
            layout="row"
            trigger={t("integrations.newSecret")}
            triggerName={t("integrations.newSecretFor", { name: e.name })}
            title={t("integrations.replaceTitle")}
            consequence={t("integrations.replaceConsequence")}
            confirmLabel={t("integrations.replace")}
            onConfirm={() => onNewSecret(e.id)}
            describeError={describeError}
          />
          <InlineConfirm
            layout="row"
            trigger={t("integrations.delete")}
            triggerName={t("integrations.deleteName", { name: e.name })}
            title={t("integrations.deleteTitle")}
            consequence={t("integrations.deleteConsequence")}
            confirmLabel={t("integrations.delete")}
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
      label={t("integrations.title")}
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
  // The stored URL's path is never shown (ADR-0123): empty keeps it.
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
      setError(t("integrations.chooseEvent"));
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
        // A new URL gets a new secret, shown like a new integration's.
        const saved = await api.updateIntegration(endpoint.id, body);
        onSaved(
          saved.secret === null
            ? {}
            : { secret: { endpoint: saved.endpoint, secret: saved.secret } },
        );
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
        {endpoint ? t("integrations.editName", { name: endpoint.name }) : t("integrations.add")}
      </h2>
      {field(
        t("integrations.name"),
        <input
          className={fieldClass}
          required
          maxLength={80}
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder={t("integrations.namePlaceholder")}
        />,
      )}
      {field(
        t("integrations.url"),
        <input
          className={fieldClass}
          type="url"
          required={!endpoint}
          spellCheck={false}
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder={
            endpoint
              ? t("integrations.urlKeep", { origin: endpoint.origin })
              : "https://psa.example.com/webhooks/monitorflow"
          }
        />,
        endpoint && url.trim() !== ""
          ? t("integrations.urlNoteNew")
          : t("integrations.urlNote"),
      )}
      {scopes.length > 1
        ? field(
            t("integrations.about"),
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
            scope && integrationNote(scope, several, customers, sites),
          )
        : scope?.value !== "everything" &&
          scope && (
            <p className={styles.fieldNote}>
              {integrationNote(scope, several, customers, sites)}
            </p>
          )}
      <fieldset className={styles.fieldset}>
        <legend className={styles.legend}>{t("integrations.events")}</legend>
        <Switch
          checked={allEvents.every((e) => events.includes(e))}
          onChange={(all) => setEvents(all ? allEvents : [])}
          label={t("integrations.allEvents")}
        />
        <div className={own.events}>
          {EVENT_GROUPS.map((group) => (
            <fieldset key={group.group} className={styles.fieldset}>
              <legend className={styles.legend}>{groupTitle(group.group)}</legend>
              {group.events
                .filter((e) => allEvents.includes(e.event))
                .map((e) => (
                  <Switch
                    key={e.event}
                    checked={events.includes(e.event)}
                    onChange={(on) => toggle(e.event, on)}
                    label={eventLabel(e.event)}
                  />
                ))}
            </fieldset>
          ))}
        </div>
      </fieldset>
      {global ? (
        <Switch
          checked={allowPrivate}
          onChange={setAllowPrivate}
          label={t("integrations.allowPrivate")}
        />
      ) : (
        keepsPrivate && (
          <p className={styles.fieldNote}>
            {t("integrations.keepsPrivate")}
          </p>
        )
      )}
      <div className={styles.formFoot}>
        <Button type="submit" variant="k" disabled={busy || !scope}>
          {busy ? t("integrations.saving") : t("integrations.save")}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          {t("integrations.cancel")}
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
    endpoint.state === "on" ? "" : ` ${t("integrations.waitsUntilOn")}`;

  async function sendAgain(body: Parameters<typeof api.replayIntegration>[1]) {
    setNotice(null);
    try {
      const { queued } = await api.replayIntegration(endpoint.id, body);
      setNotice({
        tone: "ok",
        text: `${t("integrations.sentAgain", { count: queued })}${waits}`,
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
      header: t("integrations.column.when"),
      cell: (d) => <RelativeTime value={d.occurred_at} now={now} />,
    },
    {
      key: "event",
      header: t("integrations.column.event"),
      cell: (d) => (
        <>
          {eventName(d.event)}
          {d.replay && <span className={styles.dim}> · {t("integrations.sentAgainMark")}</span>}
        </>
      ),
    },
    {
      key: "result",
      header: t("integrations.column.result"),
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
          aria-label={t("integrations.sendNameAgain", { event: eventName(d.event) })}
          onClick={() => void sendAgain({ event_id: d.event_id })}
        >
          {t("integrations.sendAgain")}
        </Button>
      ),
    },
  ];
  return (
    <section className={styles.section} aria-labelledby={`${id}-title`}>
      <div className={styles.sectionHead}>
        <h2 id={`${id}-title`} className={styles.sectionTitle}>
          {t("integrations.deliveriesTo", { name: endpoint.name })}
        </h2>
        <Button size="small" onClick={onClose}>
          {t("integrations.close")}
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
            {t("integrations.from")}
            <input
              className={fieldClass}
              type="datetime-local"
              required
              value={from}
              onChange={(e) => setFrom(e.target.value)}
            />
          </label>
          <label className={styles.field}>
            {t("integrations.to")}
            <input
              className={fieldClass}
              type="datetime-local"
              value={to}
              onChange={(e) => setTo(e.target.value)}
            />
          </label>
          <Button type="submit">{t("integrations.sendRange")}</Button>
        </form>
        <p className={styles.fieldNote}>
          {t("integrations.rangeNote")}
        </p>
      </div>
      {!data ? (
        error ? (
          <ErrorState
            what={t("integrations.deliveriesWhat")}
            error={describeError(error)}
            onRetry={refresh}
          />
        ) : (
          <LoadingBlock />
        )
      ) : rows.length === 0 ? (
        <p className={styles.dim}>
          {t("integrations.nothingYet")}
        </p>
      ) : (
        <>
          <DataTable
            bare
            label={t("integrations.deliveriesTo", { name: endpoint.name })}
            columns={columns}
            rows={rows}
            rowKey={(d) => String(d.id)}
          />
          {more != null && (
            <Button onClick={() => void loadOlder()}>{t("integrations.older")}</Button>
          )}
        </>
      )}
    </section>
  );
}
