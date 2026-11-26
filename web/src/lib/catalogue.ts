import { type PlainKey, t } from "@/i18n";

import { type CheckType, CHECK_TYPES, checkName, pluginCheckTypes } from "./alerts";
import { type CheckUsage, type UsablePlugin } from "./api";

/**
 * The check catalogue: every check one can add, built-in kinds and the
 * usual things they watch (a PostgreSQL port, the nginx service), plus the
 * check plugins turned on where the check goes. Each entry opens the Add a
 * check form with its values filled in; nothing here is a new kind of check.
 */

export type Purpose = "availability" | "performance" | "disk" | "services" | "logs" | "network" | "updates" | "databases" | "web" | "containers";
export type System = "linux" | "windows" | "proxmox";
export type Technology = "agent" | "script" | "port" | "website" | "application" | "plugin";

/** A filter choice whose label is read in the active language when shown. */
const option = <T extends string>(value: T, key: PlainKey) => ({
  value,
  get label() {
    return t(key);
  },
});

export const PURPOSES: { value: Purpose; label: string }[] = [
  option("availability", "checks.purpose.availability"),
  option("performance", "checks.purpose.performance"),
  option("disk", "checks.purpose.disk"),
  option("services", "checks.purpose.services"),
  option("databases", "checks.purpose.databases"),
  option("web", "checks.purpose.web"),
  option("containers", "checks.purpose.containers"),
  option("logs", "checks.purpose.logs"),
  option("network", "checks.purpose.network"),
  option("updates", "checks.purpose.updates"),
];

export const SYSTEMS: { value: System; label: string }[] = [
  { value: "linux", label: "Linux" },
  { value: "windows", label: "Windows" },
  { value: "proxmox", label: "Proxmox" },
];

export const TECHNOLOGIES: { value: Technology; label: string }[] = [
  option("agent", "checks.technology.agent"),
  option("port", "checks.technology.port"),
  option("website", "checks.technology.website"),
  option("application", "checks.technology.application"),
  option("script", "checks.technology.script"),
  option("plugin", "checks.technology.plugin"),
];

export interface CatalogueEntry {
  id: string;
  title: string;
  description: string;
  purposes: Purpose[];
  systems: System[];
  technology: Technology;
  /** Other words people search with. */
  keywords: string[];
  /** The form it opens, with the entry's values prefilled; none for a check every server already runs. */
  type: CheckType | null;
}

const kind = (k: string) => CHECK_TYPES.find((t) => t.kind === k)!;

/** `base` with the entry's title and values filled in, and per-field hints. */
function prefill(base: CheckType, title: string, values: Record<string, string>, hints: Record<string, string> = {}): CheckType {
  return {
    ...base,
    label: title,
    fields: base.fields.map((f) => ({
      ...f,
      ...(values[f.key] !== undefined && { initial: values[f.key] }),
      ...(hints[f.key] !== undefined && { hint: hints[f.key] }),
    })),
  };
}

type Template = Omit<CatalogueEntry, "type" | "keywords" | "technology" | "title" | "description"> & { keywords?: string[] };

/** An entry's words in the active language, by its ID. */
const text = (id: string, part: "title" | "description" | "hint") => t(`checks.entry.${id}.${part}` as PlainKey);

/**
 * An entry whose title, description and prefilled form are read when
 * shown, so the list is built once and follows the language.
 */
function entry(
  spec: Template,
  technology: Technology,
  keywords: string[],
  type: (title: string) => CheckType | null,
  title: () => string = () => text(spec.id, "title"),
): CatalogueEntry {
  return {
    ...spec,
    keywords,
    technology,
    get title() {
      return title();
    },
    get description() {
      return text(spec.id, "description");
    },
    get type() {
      return type(title());
    },
  };
}

/** One of the kinds of check itself, titled as the Add a check form names it. */
const plainKind = (spec: Template, technology: Technology, name: string): CatalogueEntry =>
  entry(spec, technology, spec.keywords ?? [], () => kind(name), () => kind(name).label);

const service = (spec: Template, unit: string, hint = false): CatalogueEntry =>
  entry(spec, "agent", spec.keywords ?? [], (title) =>
    prefill(kind("systemd"), title, { units: unit }, hint ? { units: text(spec.id, "hint") } : {}),
  );

const winService = (spec: Template, name: string): CatalogueEntry =>
  entry(spec, "agent", spec.keywords ?? [], (title) => prefill(kind("windows_service"), title, { services: name }));

const port = (spec: Template, number: number): CatalogueEntry =>
  entry(spec, "port", [...(spec.keywords ?? []), String(number), "tcp"], (title) =>
    prefill(kind("port"), title, { port: String(number) }),
  );

const logLines = (spec: Template, path: string, pattern: string): CatalogueEntry =>
  entry(spec, "agent", spec.keywords ?? [], (title) => prefill(kind("log"), title, { path, pattern }));

const eventLog = (spec: Template, channel: string): CatalogueEntry =>
  entry(spec, "agent", spec.keywords ?? [], (title) => prefill(kind("eventlog"), title, { channel }));

/** A website check with some of its fields first, the rest under More options (ADR-0150). */
const website = (spec: Template, first: string[], values: Record<string, string> = {}): CatalogueEntry =>
  entry(spec, "website", spec.keywords ?? [], (title) => {
    const type = prefill(kind("http"), title, values);
    return { ...type, fields: type.fields.map((f) => ({ ...f, advanced: !first.includes(f.key) })) };
  });

const always = (spec: Template): CatalogueEntry => entry(spec, "agent", spec.keywords ?? [], () => null);

/** Proxmox VE is Debian: Linux checks run there too, and a Linux server is offered the Proxmox ones. */
const LINUX: System[] = ["linux"];
const ANY: System[] = ["linux", "windows", "proxmox"];

export const CATALOGUE: CatalogueEntry[] = [
  // What every server runs already: found by a search, never added twice.
  always({ id: "cpu", purposes: ["performance"], systems: ANY, keywords: ["processor", "load"] }),
  always({ id: "memory", purposes: ["performance"], systems: ANY, keywords: ["ram", "swap", "page file"] }),
  always({ id: "disk", purposes: ["disk"], systems: ANY, keywords: ["free space", "full", "filesystem", "mount"] }),
  always({ id: "diskio", purposes: ["disk", "performance"], systems: ANY, keywords: ["io", "latency", "wait"] }),

  // The kinds themselves, for anything the templates do not name.
  plainKind({ id: "port", purposes: ["availability", "network"], systems: ANY, keywords: ["tcp", "reachable", "connect"] }, "port", "port"),
  plainKind({ id: "service", purposes: ["services", "availability"], systems: LINUX, keywords: ["systemd", "unit", "daemon"] }, "agent", "systemd"),
  plainKind({ id: "windows-service", purposes: ["services", "availability"], systems: ["windows"], keywords: ["service"] }, "agent", "windows_service"),
  plainKind({ id: "log", purposes: ["logs"], systems: ANY, keywords: ["pattern", "regex", "grep"] }, "agent", "log"),
  plainKind({ id: "eventlog", purposes: ["logs"], systems: ["windows"], keywords: ["event viewer"] }, "agent", "eventlog"),
  entry({ id: "script", purposes: ["availability"], systems: ANY }, "script", ["nagios", "custom", "plugin", "powershell"], () => kind("script")),

  // Websites, as a visitor sees them.
  plainKind({ id: "website", purposes: ["web", "availability"], systems: ANY, keywords: ["http", "https", "url", "page", "status code", "redirect", "ssl", "tls", "certificate", "response time", "site"] }, "website", "http"),
  website({ id: "website-text", purposes: ["web"], systems: ANY, keywords: ["content", "text", "keyword", "string", "error page"] }, ["url", "contains", "not_contains"]),
  website({ id: "website-certificate", purposes: ["web", "availability"], systems: ANY, keywords: ["ssl", "tls", "https", "certificate", "expiry", "expires", "renewal", "let's encrypt"] }, ["url"]),
  website({ id: "website-api", purposes: ["web", "availability"], systems: ANY, keywords: ["api", "rest", "json", "endpoint", "health", "token", "status code"] }, ["url", "status", "headers", "username", "password"], { status: "200" }),

  // Ready-made application checks, with their alerts (ADR-0183).
  plainKind({ id: "postgresql", purposes: ["databases", "performance", "availability"], systems: ANY, keywords: ["postgres", "sql", "database", "replication", "connections", "locks", "slow queries", "pg_stat"] }, "application", "postgresql"),
  plainKind({ id: "mysql", purposes: ["databases", "performance", "availability"], systems: ANY, keywords: ["mariadb", "sql", "database", "replication", "innodb", "slow queries", "connections"] }, "application", "mysql"),
  plainKind({ id: "redis", purposes: ["databases", "performance", "availability"], systems: ANY, keywords: ["cache", "key value", "memory", "evictions", "valkey", "keydb"] }, "application", "redis"),
  plainKind({ id: "jmx", purposes: ["performance", "availability"], systems: ANY, keywords: ["java", "jvm", "jmx", "jolokia", "heap", "garbage collection", "gc", "threads", "tomcat", "kafka", "spring boot"] }, "application", "jmx"),
  plainKind({ id: "docker", purposes: ["containers", "availability", "performance"], systems: LINUX, keywords: ["container", "containers", "restarts", "health", "podman", "compose"] }, "application", "docker"),

  // Linux services.
  service({ id: "ssh-service", purposes: ["services", "availability"], systems: LINUX, keywords: ["sshd", "openssh", "remote access"] }, "ssh", true),
  service({ id: "cron-service", purposes: ["services"], systems: LINUX, keywords: ["crond", "scheduled", "jobs"] }, "cron", true),
  service({ id: "time-service", purposes: ["services", "network"], systems: LINUX, keywords: ["ntp", "chrony", "timesyncd", "clock"] }, "chrony", true),
  service({ id: "nginx-service", purposes: ["services", "web"], systems: LINUX, keywords: ["http", "reverse proxy"] }, "nginx"),
  service({ id: "apache-service", purposes: ["services", "web"], systems: LINUX, keywords: ["httpd", "apache2", "http"] }, "apache2", true),
  service({ id: "postgresql-service", purposes: ["services", "databases"], systems: LINUX, keywords: ["postgres", "sql", "database"] }, "postgresql"),
  service({ id: "mysql-service", purposes: ["services", "databases"], systems: LINUX, keywords: ["mariadb", "sql", "database"] }, "mariadb", true),
  service({ id: "redis-service", purposes: ["services", "databases"], systems: LINUX, keywords: ["cache", "key value"] }, "redis-server", true),
  service({ id: "mongodb-service", purposes: ["services", "databases"], systems: LINUX, keywords: ["mongod", "nosql", "database"] }, "mongod"),
  service({ id: "docker-service", purposes: ["services", "containers"], systems: LINUX, keywords: ["dockerd", "container engine"] }, "docker"),
  service({ id: "containerd-service", purposes: ["services", "containers"], systems: LINUX, keywords: ["kubernetes", "runtime"] }, "containerd"),
  service({ id: "postfix-service", purposes: ["services"], systems: LINUX, keywords: ["mail", "smtp", "mta"] }, "postfix"),
  service({ id: "fail2ban-service", purposes: ["services", "network"], systems: LINUX, keywords: ["security", "ban", "brute force"] }, "fail2ban"),
  service({ id: "pve-cluster-service", purposes: ["services", "availability"], systems: ["proxmox"], keywords: ["pve", "cluster", "pmxcfs"] }, "pve-cluster"),
  service({ id: "pveproxy-service", purposes: ["services", "web"], systems: ["proxmox"], keywords: ["pve", "api", "gui"] }, "pveproxy"),

  // Windows services.
  winService({ id: "iis-service", purposes: ["services", "web"], systems: ["windows"], keywords: ["w3svc", "http", "web server"] }, "W3SVC"),
  winService({ id: "mssql-service", purposes: ["services", "databases"], systems: ["windows"], keywords: ["mssql", "database", "sql"] }, "MSSQLSERVER"),
  winService({ id: "ad-service", purposes: ["services", "availability"], systems: ["windows"], keywords: ["ntds", "domain controller", "ldap"] }, "NTDS"),
  winService({ id: "dns-service", purposes: ["services", "network"], systems: ["windows"], keywords: ["dns"] }, "DNS"),
  winService({ id: "w32time-service", purposes: ["services", "network"], systems: ["windows"], keywords: ["ntp", "time", "clock"] }, "W32Time"),
  winService({ id: "wuauserv-service", purposes: ["services", "updates"], systems: ["windows"], keywords: ["wuauserv", "patches"] }, "wuauserv"),
  winService({ id: "spooler-service", purposes: ["services"], systems: ["windows"], keywords: ["printer", "print server"] }, "Spooler"),

  // Ports.
  port({ id: "ssh-port", purposes: ["availability", "network"], systems: LINUX, keywords: ["ssh", "remote access"] }, 22),
  port({ id: "http-port", purposes: ["availability", "web"], systems: ANY, keywords: ["http", "website"] }, 80),
  port({ id: "https-port", purposes: ["availability", "web"], systems: ANY, keywords: ["https", "tls", "ssl", "website"] }, 443),
  port({ id: "postgresql-port", purposes: ["availability", "databases"], systems: LINUX, keywords: ["postgres", "database"] }, 5432),
  port({ id: "mysql-port", purposes: ["availability", "databases"], systems: ANY, keywords: ["mariadb", "database"] }, 3306),
  port({ id: "mssql-port", purposes: ["availability", "databases"], systems: ["windows"], keywords: ["mssql", "database"] }, 1433),
  port({ id: "redis-port", purposes: ["availability", "databases"], systems: LINUX, keywords: ["cache"] }, 6379),
  port({ id: "mongodb-port", purposes: ["availability", "databases"], systems: LINUX, keywords: ["mongod", "database"] }, 27017),
  port({ id: "smtp-port", purposes: ["availability", "network"], systems: ANY, keywords: ["mail", "smtp", "postfix", "exchange"] }, 25),
  port({ id: "dns-port", purposes: ["availability", "network"], systems: ANY, keywords: ["dns", "bind", "named"] }, 53),
  port({ id: "ldap-port", purposes: ["availability", "network"], systems: ANY, keywords: ["directory", "active directory"] }, 389),
  port({ id: "smb-port", purposes: ["availability", "network"], systems: ANY, keywords: ["smb", "cifs", "samba", "file server", "share"] }, 445),
  port({ id: "nfs-port", purposes: ["availability", "network", "disk"], systems: LINUX, keywords: ["nfs", "file server", "share"] }, 2049),
  port({ id: "rdp-port", purposes: ["availability", "network"], systems: ["windows"], keywords: ["rdp", "remote access"] }, 3389),
  port({ id: "proxmox-port", purposes: ["availability", "web"], systems: ["proxmox"], keywords: ["pve", "api", "gui"] }, 8006),

  // Logs.
  logLines({ id: "syslog-errors", purposes: ["logs"], systems: LINUX, keywords: ["system log"] }, "/var/log/syslog", "error"),
  logLines({ id: "ssh-failures", purposes: ["logs", "network"], systems: LINUX, keywords: ["security", "brute force", "auth"] }, "/var/log/auth.log", "Failed password"),
  logLines({ id: "oom-kills", purposes: ["logs", "performance"], systems: LINUX, keywords: ["oom", "kernel", "memory"] }, "/var/log/kern.log", "Out of memory|oom-kill"),
  logLines({ id: "nginx-errors", purposes: ["logs", "web"], systems: LINUX, keywords: ["http", "error log"] }, "/var/log/nginx/error.log", "\\[(crit|alert|emerg)\\]"),
  eventLog({ id: "system-events", purposes: ["logs"], systems: ["windows"], keywords: ["event viewer"] }, "System"),
  eventLog({ id: "application-events", purposes: ["logs"], systems: ["windows"], keywords: ["event viewer"] }, "Application"),

  // Scripts people already have.
  entry({ id: "apt-updates", purposes: ["updates"], systems: LINUX }, "script", ["apt", "patches", "nagios", "security updates"], (title) =>
    prefill(kind("script"), title, { script: "check_apt" }, { script: text("apt-updates", "hint") }),
  ),
];

/** Shown as "Common checks" while nothing is used yet. */
export const COMMON: string[] = ["website", "ssh-service", "port", "service", "log", "script"];

/** The entries for check plugins turned on at the site (ADR-0103). */
export function pluginEntries(plugins: UsablePlugin[], siteId: string, customerId: string | null): CatalogueEntry[] {
  const types = pluginCheckTypes(plugins, siteId, customerId);
  return types.map((type) => {
    const plugin = plugins.find((p) => p.name === type.fixed?.plugin)!;
    return {
      id: `plugin:${plugin.name}`,
      title: plugin.title,
      description: plugin.description,
      purposes: [],
      systems: ANY,
      technology: "plugin" as const,
      keywords: [plugin.name, ...plugin.capabilities],
      type,
    };
  });
}

/** Which entries a server of `osFamily` runs; every entry for a site (`null`). */
export function entriesFor(entries: CatalogueEntry[], osFamily: string | null): CatalogueEntry[] {
  if (osFamily === null) return entries;
  const windows = osFamily === "windows";
  return entries
    .filter((e) => e.systems.some((s) => (s === "windows") === windows))
    .map((e) => {
      if (!e.type) return e;
      const texts = e.type.on?.[windows ? "windows" : "linux"];
      return texts ? { ...e, type: { ...e.type, fields: e.type.fields.map((f) => ({ ...f, ...texts[f.key] })) } } : e;
    });
}

export interface Filters {
  query: string;
  purpose: Purpose | null;
  system: System | null;
  technology: Technology | null;
}

export const NO_FILTERS: Filters = { query: "", purpose: null, system: null, technology: null };

const fold = (text: string) => text.normalize("NFD").replace(/\p{Diacritic}/gu, "").toLowerCase();

const label = <T extends string>(list: { value: T; label: string }[], value: T) => list.find((l) => l.value === value)?.label ?? value;

/** Every word of the search is somewhere in the entry: title, description, keywords or its filter names. */
function matchesQuery(entry: CatalogueEntry, query: string): boolean {
  const words = fold(query).split(/\s+/).filter(Boolean);
  if (words.length === 0) return true;
  const haystack = fold(
    [
      entry.title,
      entry.description,
      ...entry.keywords,
      ...entry.purposes.map((p) => label(PURPOSES, p)),
      ...entry.systems.map((s) => label(SYSTEMS, s)),
      label(TECHNOLOGIES, entry.technology),
    ].join(" "),
  );
  return words.every((w) => haystack.includes(w));
}

/** The entries passing `filters`. */
export function filterEntries(entries: CatalogueEntry[], filters: Filters): CatalogueEntry[] {
  return entries.filter(
    (e) =>
      matchesQuery(e, filters.query) &&
      (filters.purpose === null || e.purposes.includes(filters.purpose)) &&
      (filters.system === null || e.systems.includes(filters.system)) &&
      (filters.technology === null || e.technology === filters.technology),
  );
}

/** The sensor name an entry's form makes when left as it is, which usage is counted by. */
function usageKey(entry: CatalogueEntry): string | null {
  const type = entry.type;
  if (!type || type.fields.some((f) => !f.initial)) return null;
  const values = Object.fromEntries(type.fields.map((f) => [f.key, f.initial!]));
  return `${type.kind}/${checkName(type, values)}`;
}

/**
 * Up to `limit` entries ranked by how many servers run them: a template by
 * the check its form makes, a plain kind or a plugin by every check of it
 * no template accounts for. Empty when nothing is used yet.
 */
export function mostUsed(entries: CatalogueEntry[], usage: CheckUsage[], limit = 6): CatalogueEntry[] {
  const byKey = new Map<string, number>();
  for (const u of usage) byKey.set(`${u.kind}/${u.name}`, (byKey.get(`${u.kind}/${u.name}`) ?? 0) + u.servers);
  const named = new Set(entries.map(usageKey).filter((k): k is string => k !== null));
  const count = (entry: CatalogueEntry): number => {
    const key = usageKey(entry);
    if (key) return byKey.get(key) ?? 0;
    const type = entry.type;
    if (!type) return 0;
    return usage
      .filter((u) => u.kind === type.kind && !named.has(`${u.kind}/${u.name}`))
      .filter((u) => type.kind !== "plugin" || u.plugin === type.fixed?.plugin)
      .reduce((sum, u) => sum + u.servers, 0);
  };
  return entries
    .map((entry) => ({ entry, n: count(entry) }))
    .filter(({ n }) => n > 0)
    .sort((a, b) => b.n - a.n)
    .slice(0, limit)
    .map(({ entry }) => entry);
}

/** "Linux, Proxmox · Port": the facts under an entry's title. */
export function entryFacts(entry: CatalogueEntry): string {
  const systems = entry.systems.length === SYSTEMS.length ? t("checks.anySystem") : entry.systems.map((s) => label(SYSTEMS, s)).join(", ");
  return `${systems} · ${label(TECHNOLOGIES, entry.technology)}`;
}
