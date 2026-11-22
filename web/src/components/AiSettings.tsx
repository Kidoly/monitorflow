"use client";

import Link from "next/link";
import { type FormEvent, Fragment, useCallback, useEffect, useId, useState } from "react";

import { rich, t } from "@/i18n";
import {
  type AiForm as AiFormBody,
  type AiModel,
  type AiProvider,
  type AiSettings,
  type ChannelTestResult,
  type Server,
  type SiteSourcesChange,
  ApiError,
  api,
  describeError,
} from "@/lib/api";
import { type Acceptance, aiCommands } from "@/lib/commandKinds";
import { administersEverything, useMe } from "@/lib/me";
import { AI_COMMANDS_SECTION, serverHref } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";
import { SegmentedControl } from "@/components/ui/SegmentedControl";

import styles from "./AdminPages.module.css";
import { Button, fieldClass } from "./ui/Button";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner, ErrorState, LoadingBlock, NoPermission, PageHeader } from "./ui/PageState";
import { SwitchResult } from "./SiteAiCommands";
import { Label } from "./ui/StatusLabel";

/** `/settings/ai/`: global administrators only, like the API (ADR-0064). */
export function AiSettingsPage() {
  const me = useMe();
  if (!administersEverything(me)) {
    return (
      <NoPermission title={t("ai.needAdmin")}>{t("ai.needAdminBody")}</NoPermission>
    );
  }
  return <Ai />;
}

/** The choices of AI, in their order on the page; their words are `ai.provider.*` (ADR-0096). */
const PROVIDERS: AiProvider[] = ["anthropic", "openai", "openai_compatible"];

/** Who makes the key of a hosted provider: "Anthropic API key". */
const VENDORS: Record<Exclude<AiProvider, "openai_compatible">, string> = { anthropic: "Anthropic", openai: "OpenAI" };

const OPENAI_MODEL = "gpt-5.1";

/** `ollama.lan:11434` of `http://ollama.lan:11434/v1`. */
function host(url: string | null): string {
  try {
    return url ? new URL(url).host : "";
  } catch {
    return url ?? "";
  }
}

/** What AI runs with, for a configuration the form does not show. */
function describeAi(settings: AiSettings): string {
  switch (settings.provider) {
    case "anthropic":
      return settings.models.find((m) => m.id === settings.model)?.name ?? settings.model;
    case "openai":
      return t("ai.fromOpenAi", { model: settings.model });
    case "openai_compatible":
      return t("ai.onHost", { model: settings.model, host: host(settings.base_url) });
  }
}

/** When to pick `model`, in the page's language; the server's English for a code this page does not know. */
function modelNote(model: AiModel | undefined): string | undefined {
  if (!model) return undefined;
  switch (model.fit) {
    case "recommended":
      return t("ai.fit.recommended");
    case "half_price":
      return t("ai.fit.half_price");
    case "cheapest":
      return t("ai.fit.cheapest");
    default:
      return model.note;
  }
}

function Ai() {
  const fetcher = useCallback((signal: AbortSignal) => api.aiSettings(signal), []);
  const { data, error, refresh } = usePolledData(fetcher);
  const [saved, setSaved] = useState<AiSettings | null>(null);
  const settings = saved ?? data;

  const header = <PageHeader title={t("ai.title")} meta={t("ai.meta")} />;
  if (!settings) {
    return (
      <div className={styles.page}>
        {header}
        {error ? <ErrorState what={t("ai.what")} error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </div>
    );
  }
  return (
    <div className={styles.page}>
      {header}
      {settings.enabled ? (
        <Banner tone="ok">
          <strong>{t("ai.on")}</strong>
        </Banner>
      ) : (
        <Banner tone="plain">{rich("ai.offBanner", { off: <strong>{t("ai.off")}</strong> })}</Banner>
      )}
      {settings.from_config ? <FromConfig settings={settings} /> : <AiForm settings={settings} onSaved={setSaved} />}
      {settings.enabled && <AllowCommands />}
    </div>
  );
}

/** The configuration wins: the page only tests what it sets. */
function FromConfig({ settings }: { settings: AiSettings }) {
  const [test, setTest] = useState<TestState>(null);
  return (
    <section className={styles.panel} aria-label={t("ai.fromConfigLabel")}>
      <p>{rich("ai.fromConfig", { section: <code>[ai]</code>, line: <code>enabled = true</code> })}</p>
      <p>{t("ai.configModel", { ai: describeAi(settings) })}</p>
      <div className={styles.formFoot}>
        <TestButton onResult={setTest} body={{ api_key: null, model: "" }} />
        <TestLine test={test} />
      </div>
    </section>
  );
}

type TestState = ChannelTestResult | "testing" | null;

function TestButton({
  body,
  disabled,
  onResult,
}: {
  body: AiFormBody;
  disabled?: boolean;
  onResult: (test: TestState) => void;
}) {
  async function run() {
    onResult("testing");
    try {
      onResult(await api.testAiSettings(body));
    } catch (e) {
      onResult({ ok: false, error: describeError(e) });
    }
  }
  return (
    <Button disabled={disabled} onClick={() => void run()}>
      {t("ai.test")}
    </Button>
  );
}

function TestLine({ test }: { test: TestState }) {
  if (test === null) return null;
  if (test === "testing") return <span className={styles.dim}>{t("ai.testing")}</span>;
  return test.ok ? (
    <span role="status">
      <Label tone="ok">{t("ai.working")}</Label> <span className={styles.dim}>{t("ai.answered")}</span>
    </span>
  ) : (
    <span role="alert">
      <Label tone="bad">{t("ai.failing")}</Label> {test.error}
    </span>
  );
}

/** Whether a typed address is the stored one, which the server keeps without a trailing slash. */
function sameAddress(typed: string, stored: string | null): boolean {
  return typed.trim().replace(/\/+$/, "") === (stored ?? "");
}

/** The model the form starts with for `provider`: the stored one when it is that provider's. */
function startModel(settings: AiSettings, provider: AiProvider): string {
  if (provider === settings.provider) return settings.model;
  if (provider === "anthropic") return settings.models[0]?.id ?? "";
  return provider === "openai" ? OPENAI_MODEL : "";
}

type ModelQuery = Omit<AiFormBody, "model">;
type ModelList = { models: string[]; error: string | null };

/** What to list models with, or null while the form cannot say. */
function modelQuery(provider: AiProvider, baseUrl: string, key: string, hasKey: boolean): ModelQuery | null {
  if (provider === "anthropic") return null;
  if (provider === "openai" && !key.trim() && !hasKey) return null;
  if (provider === "openai_compatible" && !baseUrl.trim()) return null;
  return { provider, api_key: key || null, base_url: provider === "openai_compatible" ? baseUrl : null };
}

function AiForm({ settings, onSaved }: { settings: AiSettings; onSaved: (settings: AiSettings) => void }) {
  const [provider, setProvider] = useState<AiProvider>(settings.provider);
  const [key, setKey] = useState("");
  const [baseUrl, setBaseUrl] = useState(settings.base_url ?? "");
  const [model, setModel] = useState(settings.model);
  const [test, setTest] = useState<TestState>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState<ModelQuery | null>(() =>
    modelQuery(settings.provider, settings.base_url ?? "", "", settings.has_key),
  );
  const [listed, setListed] = useState<ModelList | null>(null);
  const listId = useId();
  const local = provider === "openai_compatible";
  // The stored key belongs to the stored provider and, for a local model,
  // to the stored address: the server sends it nowhere else.
  const moved = local && settings.has_key && provider === settings.provider && !sameAddress(baseUrl, settings.base_url);
  const hasKey = settings.has_key && provider === settings.provider && !moved;
  const canUse = local ? baseUrl.trim() !== "" && model.trim() !== "" : hasKey || key.trim() !== "";
  const note = provider === "anthropic" ? modelNote(settings.models.find((m) => m.id === model)) : undefined;
  const form: AiFormBody = { provider, api_key: key || null, base_url: local ? baseUrl : null, model };

  useEffect(() => {
    if (!query) return;
    let live = true;
    api
      .aiModels(query)
      .then((list) => {
        if (!live) return;
        setListed(list);
        // A local server's first model is a good start when none is typed.
        if (query.provider === "openai_compatible") setModel((typed) => typed || list.models[0] || "");
      })
      .catch((e: unknown) => live && setListed({ models: [], error: describeError(e) }));
    return () => {
      live = false;
    };
  }, [query]);

  function changed() {
    setTest(null);
    setError(null);
  }

  /** Lists models again for what the form now holds. */
  function relist(next: { provider?: AiProvider; baseUrl?: string; key?: string } = {}) {
    const p = next.provider ?? provider;
    const q = modelQuery(p, next.baseUrl ?? baseUrl, next.key ?? key, settings.has_key && p === settings.provider);
    if (JSON.stringify(q) === JSON.stringify(query)) return;
    setListed(null);
    setQuery(q);
  }

  function choose(next: AiProvider) {
    setProvider(next);
    setKey("");
    const url = next === settings.provider ? (settings.base_url ?? "") : "";
    setBaseUrl(url);
    setModel(startModel(settings, next));
    changed();
    relist({ provider: next, baseUrl: url, key: "" });
  }

  async function save(enabled: boolean) {
    setBusy(true);
    setError(null);
    try {
      onSaved(await api.setAiSettings({ ...form, enabled }));
      setKey("");
      setTest(null);
    } catch (e) {
      setError(e instanceof ApiError && e.status === 400 ? e.message : describeError(e));
    }
    setBusy(false);
  }

  function submit(e: FormEvent) {
    e.preventDefault();
    void save(true);
  }

  const keyLabel = provider === "openai_compatible" ? t("ai.keyLocal") : t("ai.key", { vendor: VENDORS[provider] });
  const keyPlaceholder = hasKey
    ? t("ai.keySaved")
    : moved
      ? t("ai.keyAgain")
      : { anthropic: "sk-ant-…", openai: "sk-…", openai_compatible: t("ai.keyNoneLocal") }[provider];
  const keyNote = t(`ai.keyNote.${provider}`);

  return (
    <>
      <form className={styles.panel} onSubmit={submit} aria-label={t("ai.formLabel")}>
        <fieldset className={styles.fieldset}>
          <legend className={styles.legend}>{t("ai.which")}</legend>
          <SegmentedControl
            label={t("ai.which")}
            options={PROVIDERS.map((p) => ({ value: p, label: t(`ai.provider.${p}`) }))}
            value={provider}
            onChange={choose}
          />
          <span className={styles.fieldNote}>{t(`ai.providerNote.${provider}`)}</span>
        </fieldset>
        {local && (
          <label className={styles.field}>
            {t("ai.address")}
            <input
              className={fieldClass}
              type="url"
              inputMode="url"
              value={baseUrl}
              onChange={(e) => {
                setBaseUrl(e.target.value);
                changed();
              }}
              onBlur={() => relist()}
              placeholder="http://ollama.lan:11434/v1"
            />
            <span className={styles.fieldNote}>
              {baseUrl.trim().toLowerCase().startsWith("http://")
                ? t("ai.httpNote")
                : t("ai.addressNote")}
            </span>
          </label>
        )}
        <label className={styles.field}>
          {keyLabel}
          <input
            className={fieldClass}
            type="password"
            autoComplete="new-password"
            value={key}
            onChange={(e) => {
              setKey(e.target.value);
              changed();
            }}
            onBlur={() => relist()}
            placeholder={keyPlaceholder}
          />
          <span className={styles.fieldNote}>{keyNote}</span>
        </label>
        <label className={styles.field}>
          {t("ai.model")}
          {provider === "anthropic" ? (
            <select
              className={fieldClass}
              value={model}
              onChange={(e) => {
                setModel(e.target.value);
                changed();
              }}
            >
              {settings.models.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.name}
                </option>
              ))}
            </select>
          ) : (
            <>
              <input
                className={fieldClass}
                list={listId}
                value={model}
                onChange={(e) => {
                  setModel(e.target.value);
                  changed();
                }}
                placeholder={local ? "qwen3:8b" : OPENAI_MODEL}
                spellCheck={false}
              />
              <datalist id={listId}>
                {listed?.models.map((m) => <option key={m} value={m} />)}
              </datalist>
            </>
          )}
          {note && <span className={styles.fieldNote}>{note}</span>}
          {provider !== "anthropic" && <ModelsLine query={query} listed={listed} />}
        </label>
        {local && (
          <p className={styles.footnote}>
            {rich("ai.localNote", { tokens: 32_000, setting: <code>OLLAMA_CONTEXT_LENGTH=32768</code> })}
          </p>
        )}
        {error && (
          <p className={styles.formError} role="alert">
            {error}
          </p>
        )}
        <div className={styles.formFoot}>
          <Button type="submit" variant="k" disabled={busy || !canUse}>
            {settings.enabled ? t("ai.save") : t("ai.enable")}
          </Button>
          <TestButton disabled={!canUse} body={form} onResult={setTest} />
          {settings.enabled && (
            <Button disabled={busy} onClick={() => void save(false)}>
              {t("ai.turnOff")}
            </Button>
          )}
          <TestLine test={test} />
        </div>
      </form>
      {/* Outside the form: InlineConfirm is a form of its own. */}
      {settings.has_key && (
        <div className={styles.buttons}>
          <InlineConfirm
            trigger={t("ai.removeKey")}
            title={t("ai.removeTitle")}
            consequence={t("ai.removeConsequence")}
            confirmLabel={t("ai.remove")}
            onConfirm={async () => onSaved(await api.removeAiKey())}
            describeError={describeError}
          />
        </div>
      )}
    </>
  );
}

/** Whether the models the server offers could be read. */
function ModelsLine({ query, listed }: { query: ModelQuery | null; listed: ModelList | null }) {
  if (!query) return null;
  if (!listed) return <span className={styles.fieldNote}>{t("ai.readingModels")}</span>;
  if (listed.error) {
    return <span className={styles.fieldNote}>{t("ai.modelsError", { error: listed.error })}</span>;
  }
  if (listed.models.length === 0) return <span className={styles.fieldNote}>{t("ai.noModels")}</span>;
  return <span className={styles.fieldNote}>{t("ai.models", { count: listed.models.length })}</span>;
}

/**
 * Where each server stands, and AI commands on or off for every server at once (ADR-0173); per site
 * and per server from their pages (ADR-0098).
 */
function AllowCommands() {
  const fetcher = useCallback((signal: AbortSignal) => api.servers(signal), []);
  const { data, refresh } = usePolledData(fetcher);
  const [result, setResult] = useState<{ on: boolean; change: SiteSourcesChange } | null>(null);
  // Linked from the approval queue: the section shows once the settings are read, after the browser looked for it.
  useEffect(() => {
    if (window.location.hash === `#${AI_COMMANDS_SECTION}`) {
      document.getElementById(AI_COMMANDS_SECTION)?.scrollIntoView?.();
    }
  }, []);
  const action = (on: boolean) => (
    <InlineConfirm
      trigger={on ? t("ai.all.onTrigger") : t("ai.all.offTrigger")}
      triggerVariant={on ? "default" : "danger"}
      title={on ? t("ai.all.onTitle") : t("ai.all.offTitle")}
      consequence={on ? t("fleet.siteAi.onConsequence") : t("fleet.siteAi.offConsequence")}
      confirmLabel={on ? t("fleet.siteAi.onConfirm") : t("fleet.siteAi.offConfirm")}
      confirmVariant={on ? "k" : "danger"}
      pendingLabel={t("fleet.siteAi.sending")}
      onConfirm={async () => {
        setResult({ on, change: await api.switchAllAiCommands(on) });
        refresh();
      }}
      describeError={(e) => t("fleet.siteAi.failed", { error: describeError(e) })}
    />
  );
  return (
    <section id={AI_COMMANDS_SECTION} className={styles.panel} aria-label={t("ai.allowLabel")}>
      <h2 className={styles.sectionTitle}>{t("ai.allowTitle")}</h2>
      <p className={styles.footnote}>{t("ai.allowBody")}</p>
      {data && data.servers.length > 0 && <ServersAccepting servers={data.servers} />}
      {result && <SwitchResult {...result} />}
      {data && data.servers.length > 0 && (
        <div className={styles.buttons}>
          {action(true)}
          {action(false)}
        </div>
      )}
    </section>
  );
}

/** Which servers run AI's commands now, and what stops the others. */
function ServersAccepting({ servers }: { servers: Server[] }) {
  const named = (state: Acceptance) => servers.filter((s) => aiCommands(s) === state);
  const accepted = named("accepted").length;
  const groups: [string, Server[]][] = [
    // With none on, listing every server adds nothing to "none accepts".
    [t("ai.group.off"), accepted === 0 ? [] : named("off")],
    [t("ai.group.unknown"), named("unknown")],
    [t("ai.group.unsupported"), named("unsupported")],
  ];
  return (
    <>
      <p className={styles.footnote}>
        <strong>
          {accepted === 0
            ? t("ai.noneAccepting")
            : t("ai.accepting", { accepted, count: servers.length })}
        </strong>
      </p>
      {groups
        .filter(([, list]) => list.length > 0)
        .map(([title, list]) => (
          <p key={title} className={styles.footnote}>
            {title}{" "}
            {list.map((server, i) => (
              <Fragment key={server.id}>
                {i > 0 && ", "}
                <Link href={serverHref(server.id, "commands")}>{server.hostname}</Link>
              </Fragment>
            ))}
          </p>
        ))}
    </>
  );
}
