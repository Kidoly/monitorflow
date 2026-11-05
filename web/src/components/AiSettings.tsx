"use client";

import Link from "next/link";
import { type FormEvent, Fragment, useCallback, useEffect, useId, useState } from "react";

import {
  type AiForm as AiFormBody,
  type AiProvider,
  type AiSettings,
  type ChannelTestResult,
  type Server,
  ApiError,
  api,
  describeError,
} from "@/lib/api";
import { type Acceptance, aiCommands } from "@/lib/commandKinds";
import { administersEverything, useMe } from "@/lib/me";
import { serverHref } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./AdminPages.module.css";
import { AllowCommandsLine } from "./AllowCommands";
import { Button, fieldClass } from "./ui/Button";
import { cx } from "./ui/cx";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner, ErrorState, LoadingBlock, NoPermission, PageHeader } from "./ui/PageState";
import { Label } from "./ui/StatusLabel";

/** `/admin/ai/`: global administrators only, like the API (ADR-0064). */
export function AiSettingsPage() {
  const me = useMe();
  if (!administersEverything(me)) {
    return (
      <NoPermission title="AI setup needs a global administrator">
        Ask one to turn on AI for this server.
      </NoPermission>
    );
  }
  return <Ai />;
}

function modelName(settings: AiSettings, id: string): string {
  return settings.models.find((m) => m.id === id)?.name ?? id;
}

/** The choices of AI, in plain words (ADR-0096). */
const PROVIDERS: Record<AiProvider, { name: string; note: string }> = {
  anthropic: {
    name: "Anthropic (Claude)",
    note: "Claude models, run by Anthropic. Incident data, with secrets removed, is sent to Anthropic.",
  },
  openai: {
    name: "OpenAI",
    note: "GPT models, run by OpenAI. Incident data, with secrets removed, is sent to OpenAI.",
  },
  openai_compatible: {
    name: "Another or a local model",
    note:
      "A model on your own network with Ollama, llama.cpp, vLLM or LM Studio, so nothing leaves it; or any service that speaks OpenAI's API, like Mistral, LiteLLM or Azure OpenAI.",
  },
};

const OPENAI_MODEL = "gpt-5.1";

/** `ollama.lan:11434` of `http://ollama.lan:11434/v1`. */
function host(url: string | null): string {
  try {
    return url ? new URL(url).host : "";
  } catch {
    return url ?? "";
  }
}

/** What AI runs with, as the banner says it. */
function describeAi(settings: AiSettings): string {
  switch (settings.provider) {
    case "anthropic":
      return modelName(settings, settings.model);
    case "openai":
      return `${settings.model} from OpenAI`;
    case "openai_compatible":
      return `${settings.model} on ${host(settings.base_url)}`;
  }
}

function Ai() {
  const fetcher = useCallback((signal: AbortSignal) => api.aiSettings(signal), []);
  const { data, error, refresh } = usePolledData(fetcher);
  const [saved, setSaved] = useState<AiSettings | null>(null);
  const settings = saved ?? data;

  const header = <PageHeader title="AI" meta="Diagnoses incidents and proposes commands to fix them" />;
  if (!settings) {
    return (
      <div className={styles.page}>
        {header}
        {error ? <ErrorState what="the AI settings" error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </div>
    );
  }
  return (
    <div className={styles.page}>
      {header}
      {settings.enabled ? (
        <Banner tone="ok">
          <strong>AI is on</strong> with {describeAi(settings)}. It looks at each new incident and proposes
          commands; risky ones wait in Approvals, and every step is in the audit log.
        </Banner>
      ) : (
        <Banner tone="plain">
          <strong>AI is off.</strong> Pick which AI to use, enter what it needs, press Test, then Enable AI. There is
          nothing to restart.
        </Banner>
      )}
      {settings.from_config ? <FromConfig /> : <AiForm settings={settings} onSaved={setSaved} />}
      {settings.enabled && <AllowCommands />}
    </div>
  );
}

/** The configuration wins: the page only tests what it sets. */
function FromConfig() {
  const [test, setTest] = useState<TestState>(null);
  return (
    <section className={styles.panel} aria-label="Set by the configuration">
      <p>
        These settings come from <code>[ai]</code> in the server&apos;s configuration, which takes precedence over this
        page. To change them, edit that file and restart the server; to manage AI here instead, remove{" "}
        <code>enabled = true</code> from it.
      </p>
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
      Test
    </Button>
  );
}

function TestLine({ test }: { test: TestState }) {
  if (test === null) return null;
  if (test === "testing") return <span className={styles.dim}>Testing…</span>;
  return test.ok ? (
    <span role="status">
      <Label tone="ok">Working</Label> <span className={styles.dim}>The model answered.</span>
    </span>
  ) : (
    <span role="alert">
      <Label tone="bad">Failing</Label> {test.error}
    </span>
  );
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
  // The stored key belongs to the stored provider.
  const hasKey = settings.has_key && provider === settings.provider;
  const local = provider === "openai_compatible";
  const canUse = local ? baseUrl.trim() !== "" && model.trim() !== "" : hasKey || key.trim() !== "";
  const note = provider === "anthropic" ? settings.models.find((m) => m.id === model)?.note : undefined;
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

  const keyLabel = local ? "API key, only if the server asks for one" : `${PROVIDERS[provider].name.split(" ")[0]} API key`;
  const keyPlaceholder = hasKey
    ? "Saved; leave empty to keep it"
    : { anthropic: "sk-ant-…", openai: "sk-…", openai_compatible: "Usually none for a local model" }[provider];
  const keyNote = {
    anthropic: "Create one at console.anthropic.com. Stored on this server, never shown again.",
    openai: "Create one at platform.openai.com. Stored on this server, never shown again.",
    openai_compatible: "Stored on this server, never shown again.",
  }[provider];

  return (
    <>
      <form className={styles.panel} onSubmit={submit} aria-label="AI settings">
        <fieldset className={styles.fieldset}>
          <legend className={styles.legend}>Which AI</legend>
          <div className={cx(styles.radios, styles.radiosWrap)}>
            {(Object.keys(PROVIDERS) as AiProvider[]).map((p) => (
              <label key={p}>
                <input type="radio" name="provider" checked={provider === p} onChange={() => choose(p)} />{" "}
                {PROVIDERS[p].name}
              </label>
            ))}
          </div>
          <span className={styles.fieldNote}>{PROVIDERS[provider].note}</span>
        </fieldset>
        {local && (
          <label className={styles.field}>
            Address
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
                ? "Sent unencrypted over your network. Plain http:// works only to a private address; anywhere else, use https://."
                : "Where the model server's API is, ending in /v1."}
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
          Model
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
            Small local models diagnose less well than large ones. Every command they propose goes through the same
            rules and approvals, so at worst an incident reaches a person sooner. Give the model a context of 32,000
            tokens or more (for Ollama, <code>OLLAMA_CONTEXT_LENGTH=32768</code>).
          </p>
        )}
        {error && (
          <p className={styles.formError} role="alert">
            {error}
          </p>
        )}
        <div className={styles.formFoot}>
          <Button type="submit" variant="k" disabled={busy || !canUse}>
            {settings.enabled ? "Save" : "Enable AI"}
          </Button>
          <TestButton disabled={!canUse} body={form} onResult={setTest} />
          {settings.enabled && (
            <Button disabled={busy} onClick={() => void save(false)}>
              Turn off AI
            </Button>
          )}
          <TestLine test={test} />
        </div>
      </form>
      {/* Outside the form: InlineConfirm is a form of its own. */}
      {settings.has_key && (
        <div className={styles.buttons}>
          <InlineConfirm
            trigger="Remove key"
            title="Remove the API key?"
            consequence="AI turns off until a new key is entered."
            confirmLabel="Remove"
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
  if (!listed) return <span className={styles.fieldNote}>Reading the models this server offers…</span>;
  if (listed.error) {
    return <span className={styles.fieldNote}>Could not list the models, type the name instead: {listed.error}</span>;
  }
  if (listed.models.length === 0) return <span className={styles.fieldNote}>The server lists no models; type the name.</span>;
  return (
    <span className={styles.fieldNote}>
      {listed.models.length} {listed.models.length === 1 ? "model" : "models"} on this server: pick one or type its name.
    </span>
  );
}

/** The agent-side opt-in, which stays on each server (architecture §8), and where each server stands. */
function AllowCommands() {
  const fetcher = useCallback((signal: AbortSignal) => api.servers(signal), []);
  const { data } = usePolledData(fetcher);
  return (
    <section className={styles.panel} aria-label="Let AI run commands">
      <h2 className={styles.sectionTitle}>Let AI run commands on a server</h2>
      <p className={styles.footnote}>
        Each server decides for itself. Run this once on every server AI may work on; elsewhere AI&apos;s commands are
        refused and the incident is handed to a person.
      </p>
      {data && data.servers.length > 0 && <ServersAccepting servers={data.servers} />}
      <p className={styles.footnote}>On Linux servers:</p>
      <AllowCommandsLine />
      <p className={styles.footnote}>On Windows servers, in PowerShell as an administrator:</p>
      <AllowCommandsLine osFamily="windows" />
    </section>
  );
}

/** Which servers run AI's commands now, and what stops the others. */
function ServersAccepting({ servers }: { servers: Server[] }) {
  const named = (state: Acceptance) => servers.filter((s) => aiCommands(s) === state);
  const groups: [string, Server[]][] = [
    ["Off, run the line below for their system on them:", named("off")],
    ["Not known yet, they say once updated, which happens by itself:", named("unknown")],
    ["Their agent is too old for AI commands, it updates by itself:", named("unsupported")],
  ];
  return (
    <>
      <p className={styles.footnote}>
        <strong>
          {named("accepted").length} of {servers.length} {servers.length === 1 ? "server accepts" : "servers accept"} AI commands.
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
