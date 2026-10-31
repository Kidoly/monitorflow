"use client";

import { type FormEvent, useCallback, useState } from "react";

import { type AiSettings, type ChannelTestResult, ApiError, api, describeError } from "@/lib/api";
import { allowCommandsCommand } from "@/lib/install";
import { administersEverything, useMe } from "@/lib/me";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./AdminPages.module.css";
import { Button, fieldClass } from "./ui/Button";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner, ErrorState, LoadingBlock, NoPermission, PageHeader } from "./ui/PageState";
import { CopyButton } from "./ui/SecretField";
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
          <strong>AI is on</strong> with {modelName(settings, settings.model)}. It looks at each new incident and proposes
          commands; risky ones wait in Approvals, and every step is in the audit log.
        </Banner>
      ) : (
        <Banner tone="plain">
          <strong>AI is off.</strong> Paste an Anthropic API key, press Test, then Enable AI. There is nothing to restart.
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
  body: { api_key: string | null; model: string };
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
      <Label tone="ok">Working</Label> <span className={styles.dim}>The key and model answered.</span>
    </span>
  ) : (
    <span role="alert">
      <Label tone="bad">Failing</Label> {test.error}
    </span>
  );
}

function AiForm({ settings, onSaved }: { settings: AiSettings; onSaved: (settings: AiSettings) => void }) {
  const [key, setKey] = useState("");
  const [model, setModel] = useState(settings.model);
  const [test, setTest] = useState<TestState>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const canUse = settings.has_key || key.trim() !== "";
  const note = settings.models.find((m) => m.id === model)?.note;

  async function save(enabled: boolean) {
    setBusy(true);
    setError(null);
    try {
      onSaved(await api.setAiSettings({ api_key: key || null, model, enabled }));
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

  return (
    <>
      <form className={styles.panel} onSubmit={submit} aria-label="AI settings">
        <label className={styles.field}>
          Anthropic API key
          <input
            className={fieldClass}
            type="password"
            autoComplete="new-password"
            value={key}
            onChange={(e) => {
              setKey(e.target.value);
              setTest(null);
            }}
            placeholder={settings.has_key ? "Saved; leave empty to keep it" : "sk-ant-…"}
          />
          <span className={styles.fieldNote}>
            Create one at console.anthropic.com. Stored on this server, never shown again.
          </span>
        </label>
        <label className={styles.field}>
          Model
          <select
            className={fieldClass}
            value={model}
            onChange={(e) => {
              setModel(e.target.value);
              setTest(null);
            }}
          >
            {settings.models.map((m) => (
              <option key={m.id} value={m.id}>
                {m.name}
              </option>
            ))}
          </select>
          {note && <span className={styles.fieldNote}>{note}</span>}
        </label>
        {error && (
          <p className={styles.formError} role="alert">
            {error}
          </p>
        )}
        <div className={styles.formFoot}>
          <Button type="submit" variant="k" disabled={busy || !canUse}>
            {settings.enabled ? "Save" : "Enable AI"}
          </Button>
          <TestButton disabled={!canUse} body={{ api_key: key || null, model }} onResult={setTest} />
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

/** The agent-side opt-in, which stays on each server (architecture §8). */
function AllowCommands() {
  const fetcher = useCallback(
    (signal: AbortSignal) => Promise.all([api.agentInstall(signal), api.publicAddress(signal)]),
    [],
  );
  const { data } = usePolledData(fetcher);
  const command = data && allowCommandsCommand(data[0], data[1], window.location.origin);
  return (
    <section className={styles.panel} aria-label="Let AI run commands">
      <h2 className={styles.sectionTitle}>Let AI run commands on a server</h2>
      <p className={styles.footnote}>
        Each server decides for itself. Run this once on every Linux server AI may work on; elsewhere AI&apos;s commands are
        refused and the incident is handed to a person. Windows servers do not run AI commands yet.
      </p>
      {command && "text" in command && (
        <div className={styles.snippet}>
          <pre className={styles.code} aria-label="Command that allows AI commands">
            {command.text}
          </pre>
          <CopyButton text={command.text} name="Copy the command" size="small" className={styles.snippetCopy} />
        </div>
      )}
      {command && "unavailable" in command && <Banner tone="warn">{command.unavailable}</Banner>}
    </section>
  );
}
