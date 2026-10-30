"use client";

import { type FormEvent, type ReactNode, useId, useState } from "react";

import {
  ApiError,
  type ChannelTestResult,
  type Customer,
  type NotificationChannel,
  type NotificationChannelRequest,
  api,
  describeError,
} from "@/lib/api";
import { administersEverything, useMe } from "@/lib/me";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./AdminPages.module.css";
import { Button, fieldClass } from "./ui/Button";
import { type Column, DataTable } from "./ui/DataTable";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner, EmptyState, ErrorState, LoadingBlock, NoPermission, PageHeader } from "./ui/PageState";
import { RelativeTime } from "./ui/RelativeTime";
import { Label } from "./ui/StatusLabel";

/** `/admin/notifications/`: global administrators only, like the API (ADR-0060). */
export function NotificationsPage() {
  const me = useMe();
  if (!administersEverything(me)) {
    return (
      <NoPermission title="Notifications need a global administrator">
        Ask one to send incidents to your email or chat.
      </NoPermission>
    );
  }
  return <Notifications />;
}

type Kind = NotificationChannel["kind"];

/** What a channel sends to, in a few words. */
export function destination(channel: NotificationChannel): string {
  if (channel.kind === "email") {
    const { to, host, port } = channel.settings;
    return `Email to ${to.join(", ")} through ${host}:${port}`;
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
  const [editing, setEditing] = useState<Editing | null>(null);
  const [tests, setTests] = useState<Record<string, ChannelTestResult | "sending">>({});
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
    <PageHeader title="Notifications" meta="Email and webhooks, for every administrator of this server" />
  );
  if (error instanceof ApiError && error.status === 403) {
    return (
      <div className={styles.page}>
        {header}
        <NoPermission title="Notifications need a global administrator" />
      </div>
    );
  }
  if (!data) {
    return (
      <div className={styles.page}>
        {header}
        {error ? <ErrorState what="notifications" error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </div>
    );
  }

  const add = (kind: Kind) => (
    <Button variant={kind === "email" ? "k" : undefined} onClick={() => setEditing({ kind })}>
      {kind === "email" ? "Add email" : "Add webhook"}
    </Button>
  );
  const allCustomers = customers.data?.customers ?? [];
  return (
    <div className={styles.page}>
      {header}
      {actionError && <Banner tone="bad">{actionError}</Banner>}
      <p className={styles.footnote}>
        MonitorFlow sends a message when an incident opens, becomes critical or resolves, when a command waits for
        approval, and when an update or a backup fails.
      </p>
      {editing ? (
        <ChannelForm
          key={editing.channel?.id ?? editing.kind}
          editing={editing}
          customers={allCustomers}
          onCancel={() => setEditing(null)}
          onSaved={(channel) => {
            setEditing(null);
            refresh();
            void test(channel.id);
          }}
        />
      ) : data.channels.length === 0 ? (
        <EmptyState
          title="No one is notified yet"
          actions={
            <>
              {add("email")}
              {add("webhook")}
            </>
          }
        >
          Incidents only show while someone has this page open. Add an email address, or a webhook for Slack,
          Mattermost or your own tools.
        </EmptyState>
      ) : (
        <div className={styles.buttons}>
          {add("email")}
          {add("webhook")}
        </div>
      )}
      {data.channels.length > 0 && (
        <section className={styles.section} aria-label="Channels">
          <div className={styles.sectionHead}>
            <h2 className={styles.sectionTitle}>Channels</h2>
          </div>
          <Channels
            channels={data.channels}
            tests={tests}
            showCustomer={allCustomers.length > 1 || data.channels.some((c) => c.customer_id)}
            onTest={(id) => void test(id)}
            onEdit={(channel) => setEditing({ kind: channel.kind, channel })}
            onDelete={remove}
          />
        </section>
      )}
    </div>
  );
}

function Status({ channel, test }: { channel: NotificationChannel; test?: ChannelTestResult | "sending" }) {
  const now = new Date();
  if (test === "sending") return <span className={styles.dim}>Sending a test…</span>;
  if (test?.ok) {
    return (
      <>
        <Label tone="ok">Test sent</Label>{" "}
        <span className={styles.dim}>
          {channel.kind === "email" ? "Check that it arrived, and the spam folder." : "Check that it arrived."}
        </span>
      </>
    );
  }
  const error = test && !test.ok ? test.error : channel.last_error;
  if (error) {
    return (
      <>
        <Label tone="bad">Failing</Label> <span role={test ? "alert" : undefined}>{error}</span>
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
  showCustomer,
  onTest,
  onEdit,
  onDelete,
}: {
  channels: NotificationChannel[];
  tests: Record<string, ChannelTestResult | "sending">;
  showCustomer: boolean;
  onTest: (id: string) => void;
  onEdit: (channel: NotificationChannel) => void;
  onDelete: (id: string) => Promise<void>;
}) {
  const columns: Column<NotificationChannel>[] = [
    { key: "destination", header: "Sends to", className: styles.wrap, cell: (c) => <strong>{destination(c)}</strong> },
    ...(showCustomer
      ? [
          {
            key: "customer",
            header: "About",
            cell: (c: NotificationChannel) => c.customer_name ?? <span className={styles.dim}>All customers</span>,
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
          <Button size="small" disabled={tests[c.id] === "sending"} onClick={() => onTest(c.id)}>
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
  return <DataTable bare label="Notification channels" columns={columns} rows={channels} rowKey={(c) => c.id} />;
}

function ChannelForm({
  editing,
  customers,
  onCancel,
  onSaved,
}: {
  editing: Editing;
  customers: Customer[];
  onCancel: () => void;
  onSaved: (channel: NotificationChannel) => void;
}) {
  const { kind, channel } = editing;
  const email = channel?.kind === "email" ? channel.settings : null;
  const hook = channel?.kind === "webhook" ? channel.settings : null;
  const [to, setTo] = useState(email?.to.join(", ") ?? "");
  const [host, setHost] = useState(email?.host ?? "");
  const [port, setPort] = useState(String(email?.port ?? 587));
  const [username, setUsername] = useState(email?.username ?? "");
  const [from, setFrom] = useState(email?.from ?? "");
  const [url, setUrl] = useState(hook?.url ?? "");
  const [secret, setSecret] = useState("");
  const [customer, setCustomer] = useState(channel?.customer_id ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const id = useId();
  const kept = channel?.has_secret ? "Saved; leave empty to keep it" : undefined;

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    const common = { secret: secret || null, customer_id: customer || null };
    const body: NotificationChannelRequest =
      kind === "email"
        ? {
            kind,
            settings: { to: to.split(/[,;\s]+/).filter(Boolean), host, port: Number(port), username, from },
            ...common,
          }
        : { kind, settings: { url }, ...common };
    try {
      const saved = channel
        ? await api.updateNotificationChannel(channel.id, body)
        : await api.createNotificationChannel(body);
      onSaved(saved);
    } catch (e) {
      setError(e instanceof ApiError && e.status === 400 ? e.message : describeError(e));
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
    <form className={styles.panel} onSubmit={(e) => void submit(e)} aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`} className={styles.sectionTitle}>
        {channel ? "Edit" : "Add"} {kind === "email" ? "email" : "webhook"}
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
              placeholder="https://hooks.slack.com/services/…"
            />,
            "Receives a JSON POST per event. Slack and Mattermost show its text as is.",
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
      {customers.length > 1 &&
        field(
          "Notify about",
          <select className={fieldClass} value={customer} onChange={(e) => setCustomer(e.target.value)}>
            <option value="">All customers</option>
            {customers.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name} only
              </option>
            ))}
          </select>,
        )}
      <div className={styles.formFoot}>
        <Button type="submit" variant="k" disabled={busy}>
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
