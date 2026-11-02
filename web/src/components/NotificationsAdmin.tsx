"use client";

import { type FormEvent, type ReactNode, useId, useState } from "react";

import {
  ApiError,
  type ChannelTestResult,
  type ChatKind,
  type ChatSettings,
  type Customer,
  type NotificationChannel,
  type NotificationChannelRequest,
  type Site,
  api,
  describeError,
} from "@/lib/api";
import { administersEverything, isAdministrator, useMe } from "@/lib/me";
import { channelScopes, scopeName, scopeValue } from "@/lib/notifications";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./AdminPages.module.css";
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
import { Label } from "./ui/StatusLabel";

/** `/admin/notifications/`: administrators, each of what they administer, like the API (ADR-0078). */
export function NotificationsPage() {
  const me = useMe();
  if (!isAdministrator(me)) {
    return (
      <NoPermission title="Notifications need an administrator">
        Ask one to send incidents to your email or chat.
      </NoPermission>
    );
  }
  return <Notifications />;
}

type Kind = NotificationChannel["kind"];

/** What each chat service needs on screen: the URL lives in that service, so the form says where. */
const CHAT: Record<
  ChatKind,
  { name: string; placeholder: string; help: string }
> = {
  slack: {
    name: "Slack",
    placeholder: "https://hooks.slack.com/services/…",
    help: "In Slack, create an app at api.slack.com/apps, turn on Incoming Webhooks, and add a webhook to your channel.",
  },
  teams: {
    name: "Teams",
    placeholder: "https://….api.powerplatform.com/…",
    help: "In Teams, open the channel's Workflows, pick “Send webhook alerts to a channel”, and copy the URL it gives you. Old connector URLs no longer work.",
  },
  discord: {
    name: "Discord",
    placeholder: "https://discord.com/api/webhooks/…",
    help: "In Discord, open the channel's settings, then Integrations, Webhooks, New Webhook, and Copy Webhook URL.",
  },
};

function isChat(kind: Kind): kind is ChatKind {
  return kind in CHAT;
}

/** What a channel sends to, in a few words. */
export function destination(channel: NotificationChannel): string {
  if (channel.kind === "email") {
    const { to, host, port } = channel.settings;
    return `Email to ${to.join(", ")} through ${host}:${port}`;
  }
  if (channel.kind !== "webhook") {
    const { name, host } = channel.settings;
    return `${CHAT[channel.kind].name}: ${name || host}`;
  }
  try {
    return `Webhook to ${new URL(channel.settings.url).host}`;
  } catch {
    return `Webhook to ${channel.settings.url}`;
  }
}

/** A random signing secret: 32 bytes in hex. */
export function newSecret(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

interface Editing {
  kind: Kind;
  /** The channel being edited, absent for a new one. */
  channel?: NotificationChannel;
}

function Notifications() {
  const { data, error, refresh } = usePolledData(api.notificationChannels);
  const customers = usePolledData(api.customers, { intervalMs: 60_000 });
  const sites = usePolledData(api.sites, { intervalMs: 60_000 });
  const me = useMe();
  const [editing, setEditing] = useState<Editing | null>(null);
  const [tests, setTests] = useState<
    Record<string, ChannelTestResult | "sending">
  >({});
  const [actionError, setActionError] = useState<string | null>(null);

  async function test(id: string) {
    setTests((t) => ({ ...t, [id]: "sending" }));
    try {
      const result = await api.testNotificationChannel(id);
      setTests((t) => ({ ...t, [id]: result }));
    } catch (e) {
      setTests((t) => ({ ...t, [id]: { ok: false, error: describeError(e) } }));
    }
    refresh();
  }

  async function remove(id: string) {
    setActionError(null);
    await api.deleteNotificationChannel(id);
    refresh();
  }

  const header = (
    <PageHeader title="Notifications" meta="Email, chat and webhooks" />
  );
  if (error instanceof ApiError && error.status === 403) {
    return (
      <div className={styles.page}>
        {header}
        <NoPermission title="Notifications need an administrator" />
      </div>
    );
  }
  if (!data) {
    return (
      <div className={styles.page}>
        {header}
        {error ? (
          <ErrorState
            what="notifications"
            error={describeError(error)}
            onRetry={refresh}
          />
        ) : (
          <LoadingBlock />
        )}
      </div>
    );
  }

  const add = (kind: Kind) => (
    <Button
      key={kind}
      variant={kind === "email" ? "k" : undefined}
      onClick={() => setEditing({ kind })}
    >
      Add {kindName(kind)}
    </Button>
  );
  const addButtons = (
    ["email", "slack", "teams", "discord", "webhook"] as const
  ).map(add);
  const allCustomers = customers.data?.customers ?? [];
  const allSites = sites.data?.sites ?? [];
  const global = administersEverything(me);
  return (
    <div className={styles.page}>
      {header}
      {actionError && <Banner tone="bad">{actionError}</Banner>}
      <p className={styles.footnote}>
        {global
          ? "MonitorFlow sends a message when an incident opens, becomes critical or resolves, when a command waits for approval, when an update or a backup fails, and when a new version is out."
          : "MonitorFlow sends a message when an incident opens, becomes critical or resolves, when a command waits for approval, and when an update fails. This page shows the channels of what you administer."}
      </p>
      {editing ? (
        <ChannelForm
          key={editing.channel?.id ?? editing.kind}
          editing={editing}
          customers={allCustomers}
          sites={allSites}
          onCancel={() => setEditing(null)}
          onSaved={(channel) => {
            setEditing(null);
            refresh();
            void test(channel.id);
          }}
        />
      ) : data.channels.length === 0 ? (
        <EmptyState title="No one is notified yet" actions={<>{addButtons}</>}>
          Incidents only show while someone has this page open. Add an email
          address, a Slack, Teams or Discord channel, or a webhook for
          Mattermost or your own tools.
        </EmptyState>
      ) : (
        <div className={styles.buttonRow}>{addButtons}</div>
      )}
      {data.channels.length > 0 && (
        <section className={styles.section} aria-label="Channels">
          <div className={styles.sectionHead}>
            <h2 className={styles.sectionTitle}>Channels</h2>
          </div>
          <Channels
            channels={data.channels}
            tests={tests}
            showScope={
              !global ||
              allCustomers.length > 1 ||
              allSites.length > 1 ||
              data.channels.some((c) => c.customer_id)
            }
            severalCustomers={allCustomers.length > 1}
            onTest={(id) => void test(id)}
            onEdit={(channel) => setEditing({ kind: channel.kind, channel })}
            onDelete={remove}
          />
        </section>
      )}
    </div>
  );
}

function kindName(kind: Kind): string {
  return isChat(kind) ? CHAT[kind].name : kind;
}

function Status({
  channel,
  test,
}: {
  channel: NotificationChannel;
  test?: ChannelTestResult | "sending";
}) {
  const now = new Date();
  if (test === "sending")
    return <span className={styles.dim}>Sending a test…</span>;
  if (test?.ok) {
    return (
      <>
        <Label tone="ok">Test sent</Label>{" "}
        <span className={styles.dim}>
          {channel.kind === "email"
            ? "Check that it arrived, and the spam folder."
            : "Check that it arrived."}
        </span>
      </>
    );
  }
  const error = test && !test.ok ? test.error : channel.last_error;
  if (error) {
    return (
      <>
        <Label tone="bad">Failing</Label>{" "}
        <span role={test ? "alert" : undefined}>{error}</span>
      </>
    );
  }
  if (channel.last_sent_at) {
    return (
      <>
        <Label tone="ok">Working</Label>{" "}
        <span className={styles.dim}>
          last sent <RelativeTime value={channel.last_sent_at} now={now} />
        </span>
      </>
    );
  }
  return <span className={styles.dim}>Nothing sent yet</span>;
}

function Channels({
  channels,
  tests,
  showScope,
  severalCustomers,
  onTest,
  onEdit,
  onDelete,
}: {
  channels: NotificationChannel[];
  tests: Record<string, ChannelTestResult | "sending">;
  showScope: boolean;
  severalCustomers: boolean;
  onTest: (id: string) => void;
  onEdit: (channel: NotificationChannel) => void;
  onDelete: (id: string) => Promise<void>;
}) {
  const columns: Column<NotificationChannel>[] = [
    {
      key: "destination",
      header: "Sends to",
      className: styles.wrap,
      cell: (c) => <strong>{destination(c)}</strong>,
    },
    ...(showScope
      ? [
          {
            key: "scope",
            header: "About",
            cell: (c: NotificationChannel) =>
              c.customer_id ? (
                scopeName(c, severalCustomers)
              ) : (
                <span className={styles.dim}>{scopeName(c, false)}</span>
              ),
          },
        ]
      : []),
    {
      key: "status",
      header: "Status",
      className: styles.wrap,
      cell: (c) => <Status channel={c} test={tests[c.id]} />,
    },
    {
      key: "actions",
      header: "",
      cell: (c) => (
        <span className={styles.buttons}>
          <Button
            size="small"
            disabled={tests[c.id] === "sending"}
            onClick={() => onTest(c.id)}
          >
            Send test
          </Button>
          <Button size="small" onClick={() => onEdit(c)}>
            Edit
          </Button>
          <InlineConfirm
            layout="row"
            trigger="Delete"
            triggerName={`Delete ${destination(c)}`}
            title="Delete this channel?"
            consequence="Nothing more is sent to it."
            confirmLabel="Delete"
            onConfirm={() => onDelete(c.id)}
            describeError={describeError}
          />
        </span>
      ),
    },
  ];
  return (
    <DataTable
      bare
      label="Notification channels"
      columns={columns}
      rows={channels}
      rowKey={(c) => c.id}
    />
  );
}

function ChannelForm({
  editing,
  customers,
  sites,
  onCancel,
  onSaved,
}: {
  editing: Editing;
  customers: Customer[];
  sites: Site[];
  onCancel: () => void;
  onSaved: (channel: NotificationChannel) => void;
}) {
  const { kind, channel } = editing;
  const email = channel?.kind === "email" ? channel.settings : null;
  const hook = channel?.kind === "webhook" ? channel.settings : null;
  const chat =
    channel && isChat(channel.kind) ? (channel.settings as ChatSettings) : null;
  const [to, setTo] = useState(email?.to.join(", ") ?? "");
  const [host, setHost] = useState(email?.host ?? "");
  const [port, setPort] = useState(String(email?.port ?? 587));
  const [username, setUsername] = useState(email?.username ?? "");
  const [from, setFrom] = useState(email?.from ?? "");
  const [url, setUrl] = useState(hook?.url ?? "");
  const [name, setName] = useState(chat?.name ?? "");
  const [secret, setSecret] = useState("");
  const [chosen, setChosen] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const id = useId();
  const kept = channel?.has_secret
    ? "Saved; leave empty to keep it"
    : undefined;
  const scopes = channelScopes(useMe(), customers, sites, channel);
  const wanted = chosen ?? (channel ? scopeValue(channel) : scopes[0]?.value);
  const scope = scopes.find((s) => s.value === wanted);
  const service = isChat(kind) ? CHAT[kind] : null;

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (!scope) return;
    setBusy(true);
    setError(null);
    const common = {
      secret: secret || null,
      customer_id: scope.customer_id,
      site_id: scope.site_id,
    };
    const body: NotificationChannelRequest =
      kind === "email"
        ? {
            kind,
            settings: {
              to: to.split(/[,;\s]+/).filter(Boolean),
              host,
              port: Number(port),
              username,
              from,
            },
            ...common,
          }
        : isChat(kind)
          ? { kind, settings: { name }, ...common }
          : { kind, settings: { url }, ...common };
    try {
      const saved = channel
        ? await api.updateNotificationChannel(channel.id, body)
        : await api.createNotificationChannel(body);
      onSaved(saved);
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
        {channel ? "Edit" : "Add"} {kindName(kind)}
      </h2>
      {kind === "email" ? (
        <>
          {field(
            "Send to",
            <input
              className={fieldClass}
              required
              value={to}
              onChange={(e) => setTo(e.target.value)}
              placeholder="ops@example.com, oncall@example.com"
            />,
          )}
          <div className={styles.formGrid}>
            {field(
              "Mail server",
              <input
                className={fieldClass}
                required
                value={host}
                onChange={(e) => setHost(e.target.value)}
                placeholder="smtp.example.com"
              />,
            )}
            {field(
              "Port",
              <input
                className={fieldClass}
                type="number"
                min={1}
                max={65535}
                required
                value={port}
                onChange={(e) => setPort(e.target.value)}
              />,
              "587 or 465 for TLS, 25 for a local relay",
            )}
          </div>
          <div className={styles.formGrid}>
            {field(
              "Username",
              <input
                className={fieldClass}
                autoComplete="off"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                placeholder="Empty when the server needs none"
              />,
            )}
            {field(
              "Password",
              <input
                className={fieldClass}
                type="password"
                autoComplete="new-password"
                value={secret}
                onChange={(e) => setSecret(e.target.value)}
                placeholder={kept}
              />,
              "Stored on the server, never shown again",
            )}
            {field(
              "Sender address",
              <input
                className={fieldClass}
                value={from}
                onChange={(e) => setFrom(e.target.value)}
                placeholder="The username, when it is an address"
              />,
            )}
          </div>
        </>
      ) : service ? (
        <>
          {field(
            "Webhook URL",
            <input
              className={fieldClass}
              required={!channel}
              autoComplete="off"
              spellCheck={false}
              value={secret}
              onChange={(e) => setSecret(e.target.value)}
              placeholder={kept ?? service.placeholder}
            />,
            `${service.help} Stored on the server, never shown again.`,
          )}
          {field(
            "Name (optional)",
            <input
              className={fieldClass}
              autoComplete="off"
              maxLength={80}
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="#ops-alerts"
            />,
            "Tells this channel from others in the list here.",
          )}
        </>
      ) : (
        <>
          {field(
            "URL",
            <input
              className={fieldClass}
              type="url"
              required
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder="https://hooks.example.com/monitorflow"
            />,
            "Receives a JSON POST per event. Mattermost shows its text as is.",
          )}
          {field(
            "Signing secret (optional)",
            <span className={styles.buttons}>
              <input
                className={fieldClass}
                autoComplete="off"
                spellCheck={false}
                value={secret}
                onChange={(e) => setSecret(e.target.value)}
                placeholder={kept}
              />
              <Button onClick={() => setSecret(newSecret())}>Generate</Button>
            </span>,
            "Each request then carries X-MonitorFlow-Signature, so your receiver can check it came from here. Copy it into the receiver before saving: it is not shown again.",
          )}
        </>
      )}
      {scopes.length > 1
        ? field(
            "Notify about",
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
            scope?.note,
          )
        : scope?.value !== "everything" && (
            <p className={styles.fieldNote}>{scope?.note}</p>
          )}
      <div className={styles.formFoot}>
        <Button type="submit" variant="k" disabled={busy || !scope}>
          {busy ? "Saving…" : "Save and send test"}
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
