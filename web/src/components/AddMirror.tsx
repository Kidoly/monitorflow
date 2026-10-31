"use client";

import { useState } from "react";

import { ApiError, type MirrorDistribution, api, describeError } from "@/lib/api";
import { formatBytes } from "@/lib/format";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./Mirrors.module.css";
import { Button } from "./ui/Button";
import { type Column, DataTable, PrimaryCell } from "./ui/DataTable";
import { Banner, ErrorState, LoadingBlock } from "./ui/PageState";
import { Label } from "./ui/StatusLabel";

/**
 * Adding a distribution of the catalog (ADR-0067): one click, with what it
 * will take on this server's disk. What cannot be added says why on its
 * row. `onAdded` gets the distribution's name.
 */
export function AddMirror({ onAdded, onClose }: { onAdded: (name: string) => void; onClose: () => void }) {
  const { data, error, refresh } = usePolledData(api.mirrorCatalog);
  const [busy, setBusy] = useState<string | null>(null);
  const [refused, setRefused] = useState<string | null>(null);

  const add = async (distribution: MirrorDistribution) => {
    setBusy(distribution.id);
    setRefused(null);
    try {
      await api.addMirror(distribution.id);
      onAdded(distribution.name);
    } catch (e) {
      setRefused(
        e instanceof ApiError && e.status === 409
          ? e.message
          : `Cannot add ${distribution.name}. ${describeError(e)}`,
      );
      refresh();
    } finally {
      setBusy(null);
    }
  };

  const header = (
    <div className={styles.addHeader}>
      <h2 className={styles.addTitle}>Add a mirror</h2>
      <Button size="small" onClick={onClose}>
        Close
      </Button>
    </div>
  );
  if (!data) {
    return (
      <section className={styles.add} aria-label="Add a mirror">
        {header}
        {error ? <ErrorState what="the distributions" error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </section>
    );
  }

  const columns: Column<MirrorDistribution>[] = [
    {
      key: "name",
      header: "Distribution",
      cell: (d) => <PrimaryCell title={d.name} sub={d.family === "apt" ? "amd64, with updates and security" : "x86_64, BaseOS and AppStream"} />,
    },
    { key: "size", header: "Disk space", cell: (d) => `about ${formatBytes(d.size_bytes)}` },
  ];

  return (
    <section className={styles.add} aria-label="Add a mirror">
      {header}
      {!data.tools.available && (
        <Banner tone="bad">
          <strong>The mirror tools are not running.</strong> Start them on the server host with{" "}
          <code>docker compose -f deploy/docker-compose.yml up -d mirror</code>, then retry.
          {data.tools.problem && <span className={styles.dim}> ({data.tools.problem})</span>}
        </Banner>
      )}
      {refused && <Banner tone="bad">{refused}</Banner>}
      <p className={styles.note}>
        {data.disk
          ? `${formatBytes(data.disk.free_bytes)} free of ${formatBytes(data.disk.total_bytes)} on this server. `
          : ""}
        A mirror syncs every day and keeps a snapshot of each sync.
      </p>
      <DataTable
        dense
        label="Distributions"
        columns={columns}
        rows={data.distributions}
        rowKey={(d) => d.id}
        rowActions={(d) =>
          d.added ? (
            <Label tone="neutral">Added</Label>
          ) : d.problem ? (
            <span className={styles.dim}>{d.problem}</span>
          ) : (
            <Button
              size="small"
              variant="k"
              disabled={busy !== null || !data.tools.available}
              onClick={() => void add(d)}
            >
              Add <span className="visually-hidden">{d.name}</span>
            </Button>
          )
        }
      />
    </section>
  );
}
