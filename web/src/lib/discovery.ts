import { t } from "@/i18n";

import type { NetworkScan, NetworkScanHost, NetworkScanRun, ScanEvery } from "./api";

/** The ports a scan tries when not told, as the server says (ADR-0154). */
export const DEFAULT_PORTS = [22, 23, 80, 135, 139, 443, 445, 3389, 5985, 8080, 8443];

/** The entries of a list typed in a text area: commas, spaces and lines separate them. */
export function splitTargets(text: string): string[] {
  return text
    .split(/[\s,;]+/)
    .map((entry) => entry.trim())
    .filter((entry) => entry !== "");
}

const IPV4 = /^(25[0-5]|2[0-4]\d|1?\d?\d)(\.(25[0-5]|2[0-4]\d|1?\d?\d)){3}$/;
const isIp = (value: string) => IPV4.test(value) || (/^[0-9a-f:.]+$/i.test(value) && value.includes(":"));

/**
 * Whether `entry` looks like what the server takes: an IP address, a block
 * `a/prefix` or a range `a-b` or `a.b.c.d-e`. The server checks sizes and
 * refused addresses.
 */
export function validTarget(entry: string): boolean {
  const bare = entry.replace(/^\[(.*)\]$/, "$1");
  const [, base = "", prefix] = bare.match(/^(.+)\/(\d{1,3})$/) ?? [];
  if (prefix !== undefined) return isIp(base) && Number(prefix) <= (base.includes(":") ? 128 : 32);
  const [, from = "", to] = bare.match(/^(.+?)-(.+)$/) ?? [];
  if (to !== undefined) return isIp(from) && (isIp(to) || (/^\d{1,3}$/.test(to) && Number(to) <= 255));
  return isIp(bare);
}

/** The ports typed in a field, or `null` when one is not a port. */
export function parsePorts(text: string): number[] | null {
  const entries = text.split(/[\s,;]+/).filter((p) => p !== "");
  const ports = entries.map(Number);
  if (ports.some((p) => !Number.isInteger(p) || p < 1 || p > 65535)) return null;
  return [...new Set(ports)].sort((a, b) => a - b);
}

/** "Every day", "When asked". */
export function everyLabel(every: ScanEvery | null): string {
  return t(`discovery.every.${every ?? "none"}`);
}

/** Why a run failed, in words. */
export function failureText(run: NetworkScanRun): string {
  switch (run.error) {
    case "interrupted":
    case "relay_disconnected":
    case "relay_outdated":
    case "relay_removed":
      return t(`discovery.failure.${run.error}`);
    default:
      return t("discovery.failure.other", { error: run.error ?? "" });
  }
}

/** "the central server", "relay relay-paris". */
export function probeWords(scan: Pick<NetworkScan, "probe" | "probe_key">): string {
  return scan.probe === "central" ? t("discovery.view.central") : t("discovery.view.relay", { name: scan.probe_key });
}

/** Whether a run waits or runs. */
export const active = (run: NetworkScanRun | null | undefined): boolean =>
  run?.state === "queued" || run?.state === "running";

const ORDER = { agent: 0, snmp_device: 1, device: 2, monitored: 3, dismissed: 4 } as const;

/** The hosts of a result split as the page shows them, proposals first, new ones on top. */
export function sections(hosts: NetworkScanHost[]): {
  proposed: NetworkScanHost[];
  monitored: NetworkScanHost[];
  dismissed: NetworkScanHost[];
} {
  const sorted = [...hosts].sort(
    (a, b) => ORDER[a.proposal.action] - ORDER[b.proposal.action] || Number(b.new) - Number(a.new),
  );
  return {
    proposed: sorted.filter((h) => ORDER[h.proposal.action] < ORDER.monitored),
    monitored: sorted.filter((h) => h.proposal.action === "monitored"),
    dismissed: sorted.filter((h) => h.proposal.action === "dismissed"),
  };
}

/** The SNMP group of each check name of a template (ADR-0149). */
const SNMP_GROUPS: Record<string, "system" | "interfaces" | "host" | "ups" | "temperature" | "printer"> = {
  snmp: "system",
  snmp_if: "interfaces",
  snmp_host: "host",
  snmp_ups: "ups",
  snmp_temp: "temperature",
  snmp_printer: "printer",
};

/** What a proposed device is checked with, in words: "ping, port 80, SNMP". */
export function checksWords(host: NetworkScanHost): string {
  const words = host.proposal.checks.map((check) => {
    if (check.kind === "ping") return t("discovery.check.ping");
    if (check.kind === "port") return t("discovery.check.port", { port: String(check.port ?? "") });
    return t(`devices.snmp.group.${SNMP_GROUPS[check.name] ?? "system"}`);
  });
  return [...new Set(words)].join(", ");
}
