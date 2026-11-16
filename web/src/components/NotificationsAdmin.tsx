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
  type UsablePlugin,
  api,
  describeError,
} from "@/lib/api";
import { rich, t } from "@/i18n";
import { administersEverything, isAdministrator, multiCustomer, useMe } from "@/lib/me";
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

/** `/settings/notifications/`: administrators, each of what they administer, like the API (ADR-0078). */
export function NotificationsPage() {
  const me = useMe();
  if (!isAdministrator(me)) {
    return (
      <NoPermission title={t("notifications.noPermission.title")}>{t("notifications.noPermission.body")}</NoPermission>
    );
  }
  return <Notifications />;
}

type Kind = NotificationChannel["kind"];

/** What each chat service needs on screen; `chatHelp` says where its URL is. */
export const CHAT: Record<ChatKind, { name: string; placeholder: string }> = {
  slack: { name: "Slack", placeholder: "https://hooks.slack.com/services/…" },
  teams: { name: "Teams", placeholder: "https://….api.powerplatform.com/…" },
  discord: { name: "Discord", placeholder: "https://discord.com/api/webhooks/…" },
};

/** Where a chat service gives its webhook URL, in the active language: the URL lives in that service. */
export function chatHelp(kind: ChatKind): string {
  return t(`notifications.chat.${kind}.help`);
}

function isChat(kind: Kind): kind is ChatKind {
  return kind in CHAT;
}

/**
 * Whether `plugin` may be used for what `scope` covers (ADR-0103): a
 * channel for everything needs it everywhere, a customer's needs that
 * customer, a site's the site or its customer.
 */
export function pluginAllows(
  plugin: UsablePlugin,
  scope: { customer_id: string | null; site_id: string | null },
  sites: Site[],
): boolean {
  if (plugin.everywhere) return true;
  if (scope.site_id) {
    const customer = sites.find((s) => s.id === scope.site_id)?.customer_id;
    return plugin.site_ids.includes(scope.site_id) || (customer !== undefined && plugin.customer_ids.includes(customer));
  }
  return scope.customer_id !== null && plugin.customer_ids.includes(scope.customer_id);
}

/** What a channel sends to, in a few words. */
export function destination(channel: NotificationChannel): string {
  if (channel.kind === "plugin") {
    const { plugin, name } = channel.settings;
    return name ? t("notifications.named", { service: plugin, name }) : plugin;
  }
  if (channel.kind === "email") {
    const { to, host, port } = channel.settings;
    return t("notifications.emailTo", { to: to.join(", "), host, port: String(port) });
  }
  if (channel.kind !== "webhook") {
    const { name, host } = channel.settings;
    return t("notifications.named", { service: CHAT[channel.kind].name, name: name || host });
  }
  try {
    return t("notifications.webhookTo", { host: new URL(channel.settings.origin).host });
  } catch {
    return t("notifications.webhookTo", { host: channel.settings.origin });
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
  /** The plugin of a plugin channel. */
  plugin?: UsablePlugin;
}

function Notifications() {
  const { data, error, refresh } = usePolledData(api.notificationChannels);
  const customers = usePolledData(api.customers, { intervalMs: 60_000 });
  const sites = usePolledData(api.sites, { intervalMs: 60_000 });
  const usable = usePolledData(api.usablePlugins, { intervalMs: 60_000 });
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
    <PageHeader title={t("notifications.title")} meta={t("notifications.meta")} />
  );
  if (error instanceof ApiError && error.status === 403) {
    return (
      <div className={styles.page}>
        {header}
        <NoPermission title={t("notifications.noPermission.title")} />
      </div>
    );
  }
  if (!data) {
    return (
      <div className={styles.page}>
        {header}
        {error ? (
          <ErrorState
            what={t("notifications.what")}
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
      {t("notifications.add", { kind: kindName(kind) })}
    </Button>
  );
  const channelPlugins = (usable.data?.plugins ?? []).filter((p) => p.kind === "channel");
  const addButtons = [
    ...(["email", "slack", "teams", "discord", "webhook"] as const).map(add),
    ...channelPlugins.map((plugin) => (
      <Button key={`plugin:${plugin.name}`} onClick={() => setEditing({ kind: "plugin", plugin })}>
        {t("notifications.add", { kind: plugin.title })}
      </Button>
    )),
  ];
  const allCustomers = customers.data?.customers ?? [];
  const allSites = sites.data?.sites ?? [];
  const global = administersEverything(me);
  return (
    <div className={styles.page}>
      {header}
      {actionError && <Banner tone="bad">{actionError}</Banner>}
      <p className={styles.footnote}>
        {global
          ? t("notifications.intro.global")
          : t("notifications.intro.scoped")}
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
        <EmptyState title={t("notifications.empty.title")} actions={<>{addButtons}</>}>
          {t("notifications.empty.body")}
        </EmptyState>
      ) : (
        <div className={styles.buttonRow}>{addButtons}</div>
      )}
      {data.channels.length > 0 && (
        <section className={styles.section} aria-label={t("notifications.channels")}>
          <div className={styles.sectionHead}>
            <h2 className={styles.sectionTitle}>{t("notifications.channels")}</h2>
          </div>
          <Channels
            channels={data.channels}
            tests={tests}
            showScope={
              !global ||
              multiCustomer(me) ||
              allSites.length > 1 ||
              data.channels.some((c) => c.customer_id)
            }
            severalCustomers={multiCustomer(me)}
            onTest={(id) => void test(id)}
            onEdit={(channel) =>
              setEditing({
                kind: channel.kind,
                channel,
                plugin:
                  channel.kind === "plugin"
                    ? channelPlugins.find((p) => p.name === channel.settings.plugin)
                    : undefined,
              })
            }
            onDelete={remove}
          />
        </section>
      )}
    </div>
  );
}

function kindName(kind: Kind, plugin?: UsablePlugin): string {
  if (kind === "plugin") return plugin?.title ?? t("notifications.kind.plugin");
  return isChat(kind) ? CHAT[kind].name : t(`notifications.kind.${kind}`);
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
    return <span className={styles.dim}>{t("notifications.sending")}</span>;
  if (test?.ok) {
    return (
      <>
        <Label tone="ok">{t("notifications.testSent")}</Label>{" "}
        <span className={styles.dim}>
          {channel.kind === "email" ? t("notifications.checkEmail") : t("notifications.check")}
        </span>
      </>
    );
  }
  const error = test && !test.ok ? test.error : channel.last_error;
  if (error) {
    return (
      <>
        <Label tone="bad">{t("notifications.failing")}</Label>{" "}
        <span role={test ? "alert" : undefined}>{error}</span>
      </>
    );
  }
  if (channel.last_sent_at) {
    return (
      <>
        <Label tone="ok">{t("notifications.working")}</Label>{" "}
        <span className={styles.dim}>
          {rich("notifications.lastSent", { time: <RelativeTime value={channel.last_sent_at} now={now} /> })}
        </span>
      </>
    );
  }
  return <span className={styles.dim}>{t("notifications.nothingSent")}</span>;
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
      header: t("notifications.column.sendsTo"),
      className: styles.wrap,
      cell: (c) => (
        <>
          <strong>{destination(c)}</strong>
          {c.follow_up_only && <span className={styles.dim}> · {t("notifications.followUpOnly")}</span>}
        </>
      ),
    },
    ...(showScope
      ? [
          {
            key: "scope",
            header: t("notifications.column.about"),
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
      header: t("notifications.column.status"),
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
            {t("notifications.sendTest")}
          </Button>
          <Button size="small" onClick={() => onEdit(c)}>
            {t("notifications.editButton")}
          </Button>
          <InlineConfirm
            layout="row"
            trigger={t("notifications.delete")}
            triggerName={t("notifications.deleteName", { name: destination(c) })}
            title={t("notifications.delete.title")}
            consequence={t("notifications.delete.consequence")}
            confirmLabel={t("notifications.delete")}
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
      label={t("notifications.table")}
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
  const { kind, channel, plugin } = editing;
  const pluginSettings = channel?.kind === "plugin" ? channel.settings : null;
  const email = channel?.kind === "email" ? channel.settings : null;
  const hook = channel?.kind === "webhook" ? channel.settings : null;
  const chat =
    channel && isChat(channel.kind) ? (channel.settings as ChatSettings) : null;
  const [to, setTo] = useState(email?.to.join(", ") ?? "");
  const [host, setHost] = useState(email?.host ?? "");
  const [port, setPort] = useState(String(email?.port ?? 587));
  const [username, setUsername] = useState(email?.username ?? "");
  const [from, setFrom] = useState(email?.from ?? "");
  // The stored URL's path is never shown (ADR-0123): empty keeps it.
  const [url, setUrl] = useState("");
  const [name, setName] = useState(chat?.name ?? pluginSettings?.name ?? "");
  const [values, setValues] = useState<Record<string, string>>(
    () =>
      pluginSettings?.values ??
      Object.fromEntries((plugin?.settings ?? []).filter((s) => s.default).map((s) => [s.key, s.default!])),
  );
  const [secrets, setSecrets] = useState<Record<string, string>>({});
  const [secret, setSecret] = useState("");
  const [followUpOnly, setFollowUpOnly] = useState(channel?.follow_up_only ?? false);
  const [chosen, setChosen] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const id = useId();
  const kept = channel?.has_secret ? t("notifications.kept") : undefined;
  const scopes = channelScopes(useMe(), customers, sites, channel).filter(
    (s) => !plugin || pluginAllows(plugin, s, sites),
  );
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
      follow_up_only: followUpOnly,
    };
    const typed = Object.entries(secrets).filter(([, v]) => v !== "");
    const body: NotificationChannelRequest =
      kind === "plugin"
        ? {
            kind,
            settings: {
              plugin: plugin?.name ?? pluginSettings?.plugin ?? "",
              name,
              values: Object.fromEntries(Object.entries(values).filter(([, v]) => v !== "")),
            },
            ...common,
            secret: typed.length > 0 ? JSON.stringify(Object.fromEntries(typed)) : null,
          }
        : kind === "email"
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
          : { kind: "webhook", settings: { url }, ...common };
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
        {t(channel ? "notifications.edit" : "notifications.add", { kind: kindName(kind, plugin) })}
      </h2>
      {kind === "plugin" ? (
        plugin ? (
          <>
            {plugin.description && <p className={styles.fieldNote}>{plugin.description}</p>}
            {plugin.settings.map((setting) =>
              field(
                setting.required ? setting.label : t("notifications.optional", { label: setting.label }),
                setting.type === "secret" ? (
                  <input
                    className={fieldClass}
                    type="password"
                    autoComplete="new-password"
                    required={setting.required && !channel?.has_secret}
                    value={secrets[setting.key] ?? ""}
                    onChange={(e) => setSecrets((s) => ({ ...s, [setting.key]: e.target.value }))}
                    placeholder={kept}
                  />
                ) : (
                  <input
                    className={fieldClass}
                    type={setting.type === "url" ? "url" : setting.type === "number" ? "number" : "text"}
                    required={setting.required}
                    spellCheck={false}
                    value={values[setting.key] ?? ""}
                    onChange={(e) => setValues((v) => ({ ...v, [setting.key]: e.target.value }))}
                  />
                ),
                setting.type === "secret"
                  ? `${setting.help ? `${setting.help} ` : ""}${t("notifications.stored")}`
                  : setting.help,
              ),
            )}
            {field(
              t("notifications.nameOptional"),
              <input
                className={fieldClass}
                autoComplete="off"
                maxLength={80}
                value={name}
                onChange={(e) => setName(e.target.value)}
              />,
              t("notifications.nameNote"),
            )}
          </>
        ) : (
          <p className={styles.formError} role="alert">
            {t("notifications.pluginGone")}
          </p>
        )
      ) : kind === "email" ? (
        <>
          {field(
            t("notifications.sendTo"),
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
              t("notifications.mailServer"),
              <input
                className={fieldClass}
                required
                value={host}
                onChange={(e) => setHost(e.target.value)}
                placeholder="smtp.example.com"
              />,
            )}
            {field(
              t("notifications.port"),
              <input
                className={fieldClass}
                type="number"
                min={1}
                max={65535}
                required
                value={port}
                onChange={(e) => setPort(e.target.value)}
              />,
              t("notifications.portNote"),
            )}
          </div>
          <div className={styles.formGrid}>
            {field(
              t("notifications.username"),
              <input
                className={fieldClass}
                autoComplete="off"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                placeholder={t("notifications.usernamePlaceholder")}
              />,
            )}
            {field(
              t("notifications.password"),
              <input
                className={fieldClass}
                type="password"
                autoComplete="new-password"
                value={secret}
                onChange={(e) => setSecret(e.target.value)}
                placeholder={kept}
              />,
              t("notifications.storedShort"),
            )}
            {field(
              t("notifications.sender"),
              <input
                className={fieldClass}
                value={from}
                onChange={(e) => setFrom(e.target.value)}
                placeholder={t("notifications.senderPlaceholder")}
              />,
            )}
          </div>
        </>
      ) : service ? (
        <>
          {field(
            t("notifications.webhookUrl"),
            <input
              className={fieldClass}
              required={!channel}
              autoComplete="off"
              spellCheck={false}
              value={secret}
              onChange={(e) => setSecret(e.target.value)}
              placeholder={kept ?? service.placeholder}
            />,
            `${chatHelp(kind as ChatKind)} ${t("notifications.stored")}`,
          )}
          {field(
            t("notifications.nameOptional"),
            <input
              className={fieldClass}
              autoComplete="off"
              maxLength={80}
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="#ops-alerts"
            />,
            t("notifications.nameNote"),
          )}
        </>
      ) : (
        <>
          {field(
            t("notifications.url"),
            <input
              className={fieldClass}
              type="url"
              required={!hook}
              spellCheck={false}
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder={
                hook
                  ? t("notifications.urlKept", { origin: hook.origin })
                  : "https://hooks.example.com/monitorflow"
              }
            />,
            t("notifications.urlNote"),
          )}
          {field(
            t("notifications.signingSecret"),
            <span className={styles.buttons}>
              <input
                className={fieldClass}
                autoComplete="off"
                spellCheck={false}
                value={secret}
                onChange={(e) => setSecret(e.target.value)}
                placeholder={kept}
              />
              <Button onClick={() => setSecret(newSecret())}>{t("notifications.generate")}</Button>
            </span>,
            t("notifications.signingNote"),
          )}
        </>
      )}
      {scopes.length > 1
        ? field(
            t("notifications.notifyAbout"),
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
      <label>
        <input type="checkbox" checked={followUpOnly} onChange={(e) => setFollowUpOnly(e.target.checked)} />{" "}
        {t("notifications.followUpOnlyLabel")}
      </label>
      <div className={styles.formFoot}>
        <Button type="submit" variant="k" disabled={busy || !scope}>
          {busy ? t("notifications.saving") : t("notifications.saveAndTest")}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          {t("notifications.cancel")}
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
