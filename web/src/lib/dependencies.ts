import type { DependencyEnd } from "./api";
import { relayHref, serverHref } from "./routes";
import { t } from "@/i18n";

/** How an end of a dependency reads (ADR-0152): "sw-core", "ping on sw-core", "relay gw-1". */
export function endWords(end: DependencyEnd): string {
  if (end.kind === "relay") return t("dependencies.end.relay", { name: end.name });
  if (!end.check) return end.name;
  const check = end.check.instance ? `${end.check.name} (${end.check.instance})` : end.check.name;
  return t("dependencies.end.check", { check, name: end.name });
}

/** The page of an end: its server or device, or its relay. */
export function endHref(end: DependencyEnd): string {
  return end.relay_id ? relayHref(end.relay_id) : serverHref(end.server_id ?? "");
}
