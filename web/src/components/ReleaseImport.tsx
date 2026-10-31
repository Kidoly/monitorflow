"use client";

import { type FormEvent, useState } from "react";

import { ApiError, type ReleaseImported, api, describeError } from "@/lib/api";
import { importNotes, releasesPage } from "@/lib/serverUpdate";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./ServerUpdate.module.css";
import { Button, fieldClass } from "./ui/Button";
import { cx } from "./ui/cx";
import { Banner } from "./ui/PageState";

/** Refusals explain themselves; anything else gets the generic sentence. */
function failureText(error: unknown): string {
  return error instanceof ApiError && error.status < 500 && error.status > 0
    ? error.message
    : `The import failed. ${describeError(error)}`;
}

/**
 * Puts a release on the server, from files the browser uploads or a URL of
 * the site, for servers without Internet (ADR-0061).
 */
export function ReleaseImport({ source }: { source: string }) {
  const held = usePolledData(api.agentReleases);
  const [files, setFiles] = useState<File[]>([]);
  // Clearing a file input takes a new element.
  const [inputKey, setInputKey] = useState(0);
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [imported, setImported] = useState<ReleaseImported | null>(null);
  const [failure, setFailure] = useState<string | null>(null);

  async function run(work: () => Promise<ReleaseImported>) {
    setBusy(true);
    setImported(null);
    setFailure(null);
    try {
      setImported(await work());
      setFiles([]);
      setInputKey((key) => key + 1);
      setUrl("");
      held.refresh();
    } catch (e) {
      setFailure(failureText(e));
    } finally {
      setBusy(false);
    }
  }

  function submitUrl(event: FormEvent) {
    event.preventDefault();
    void run(() => api.importReleaseUrl(url));
  }

  const page = releasesPage(source);
  const versions = held.data?.versions ?? [];
  return (
    <section className={styles.box} aria-labelledby="import-title">
      <div className={styles.boxHeader}>
        <h2 id="import-title" className={styles.title}>
          Import a release
        </h2>
      </div>
      <p className={styles.note}>
        Hosts without Internet access install and update from this server.{" "}
        {held.data &&
          (versions.length > 0
            ? `It holds ${versions.join(", ")}.`
            : "It holds no release yet: import one to get started.")}
      </p>
      <p className={styles.note}>
        On a machine with Internet access, download every file of the release
        {page && (
          <>
            {" "}
            from the{" "}
            <a href={page} target="_blank" rel="noreferrer">
              releases page
            </a>
          </>
        )}
        , then choose them all here. Each file is checked against the release signature.
      </p>
      {imported && (
        <Banner tone="ok">
          Release {imported.version} is on this server.
          {importNotes(imported).map((line) => (
            <span key={line}> {line}</span>
          ))}
        </Banner>
      )}
      {failure && <Banner tone="bad">{failure}</Banner>}
      <div className={styles.importRow}>
        <input
          key={inputKey}
          type="file"
          multiple
          aria-label="Release files"
          disabled={busy}
          onChange={(e) => setFiles([...(e.target.files ?? [])])}
        />
        <Button
          variant="k"
          disabled={busy || files.length === 0}
          onClick={() => void run(() => api.importRelease(files))}
        >
          {busy ? "Importing…" : "Import"}
        </Button>
      </div>
      <form className={styles.importRow} onSubmit={submitUrl}>
        <input
          type="url"
          className={cx(fieldClass, styles.url)}
          aria-label="Release directory URL"
          placeholder="Or a directory of your site, like https://files.example/monitorflow/0.3.0/"
          value={url}
          disabled={busy}
          onChange={(e) => setUrl(e.target.value)}
        />
        <Button type="submit" disabled={busy || url.trim() === ""}>
          Import from URL
        </Button>
      </form>
    </section>
  );
}
