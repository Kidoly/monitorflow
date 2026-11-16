"use client";

import { useState } from "react";

import { rich, t } from "@/i18n";
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
          : t("mirrors.cannotAdd", { name: distribution.name, error: describeError(e) }),
      );
      refresh();
    } finally {
      setBusy(null);
    }
  };

  const header = (
    <div className={styles.addHeader}>
      <h2 className={styles.addTitle}>{t("mirrors.add")}</h2>
      <Button size="small" onClick={onClose}>
        {t("mirrors.close")}
      </Button>
    </div>
  );
  if (!data) {
    return (
      <section className={styles.add} aria-label={t("mirrors.add")}>
        {header}
        {error ? <ErrorState what={t("mirrors.distributions")} error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </section>
    );
  }

  const columns: Column<MirrorDistribution>[] = [
    {
      key: "name",
      header: t("mirrors.column.distribution"),
      cell: (d) => <PrimaryCell title={d.name} sub={d.family === "apt" ? t("mirrors.apt") : t("mirrors.dnf")} />,
    },
    { key: "size", header: t("mirrors.column.diskSpace"), cell: (d) => t("mirrors.about", { size: formatBytes(d.size_bytes) }) },
  ];

  return (
    <section className={styles.add} aria-label={t("mirrors.add")}>
      {header}
      {!data.tools.available && (
        <Banner tone="bad">
          <strong>{t("mirrors.tools.down")}</strong>{" "}
          {rich("mirrors.tools.start", { command: <code>docker compose -f deploy/docker-compose.yml up -d mirror</code> })}
          {data.tools.problem && <span className={styles.dim}> ({data.tools.problem})</span>}
        </Banner>
      )}
      {refused && <Banner tone="bad">{refused}</Banner>}
      <p className={styles.note}>
        {data.disk
          ? `${t("mirrors.free", { free: formatBytes(data.disk.free_bytes), total: formatBytes(data.disk.total_bytes) })} `
          : ""}
        {t("mirrors.addNote")}
      </p>
      <DataTable
        dense
        label={t("mirrors.distributionsLabel")}
        columns={columns}
        rows={data.distributions}
        rowKey={(d) => d.id}
        rowActions={(d) =>
          d.added ? (
            <Label tone="neutral">{t("mirrors.addedLabel")}</Label>
          ) : d.problem ? (
            <span className={styles.dim}>{d.problem}</span>
          ) : (
            <Button
              size="small"
              variant="k"
              disabled={busy !== null || !data.tools.available}
              onClick={() => void add(d)}
            >
              {t("mirrors.addButton")} <span className="visually-hidden">{d.name}</span>
            </Button>
          )
        }
      />
    </section>
  );
}
