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
export type Technology = "agent" | "script" | "port" | "plugin";

export const PURPOSES: { value: Purpose; label: string }[] = [
  { value: "availability", label: "Availability" },
  { value: "performance", label: "Performance" },
  { value: "disk", label: "Disk" },
  { value: "services", label: "Services" },
  { value: "databases", label: "Databases" },
  { value: "web", label: "Web" },
  { value: "containers", label: "Containers" },
  { value: "logs", label: "Logs" },
  { value: "network", label: "Network" },
  { value: "updates", label: "Updates" },
];

export const SYSTEMS: { value: System; label: string }[] = [
  { value: "linux", label: "Linux" },
  { value: "windows", label: "Windows" },
  { value: "proxmox", label: "Proxmox" },
];

export const TECHNOLOGIES: { value: Technology; label: string }[] = [
  { value: "agent", label: "Built into the agent" },
  { value: "port", label: "Port" },
  { value: "script", label: "Script" },
  { value: "plugin", label: "Plugin" },
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

type Template = Omit<CatalogueEntry, "type" | "keywords" | "technology"> & { keywords?: string[] };

const service = (t: Template, unit: string, hint?: string): CatalogueEntry => ({
  keywords: [],
  ...t,
  technology: "agent",
  type: prefill(kind("systemd"), t.title, { units: unit }, hint ? { units: hint } : {}),
});

const winService = (t: Template, name: string): CatalogueEntry => ({
  keywords: [],
  ...t,
  technology: "agent",
  type: prefill(kind("windows_service"), t.title, { services: name }),
});

const port = (t: Template, number: number): CatalogueEntry => ({
  ...t,
  keywords: [...(t.keywords ?? []), String(number), "tcp"],
  technology: "port",
  type: prefill(kind("port"), t.title, { port: String(number) }),
});

const logLines = (t: Template, path: string, pattern: string): CatalogueEntry => ({
  keywords: [],
  ...t,
  technology: "agent",
  type: prefill(kind("log"), t.title, { path, pattern }),
});

const eventLog = (t: Template, channel: string): CatalogueEntry => ({
  keywords: [],
  ...t,
  technology: "agent",
  type: prefill(kind("eventlog"), t.title, { channel }),
});

const always = (t: Template): CatalogueEntry => ({ keywords: [], ...t, technology: "agent", type: null });

/** Proxmox VE is Debian: Linux checks run there too, and a Linux server is offered the Proxmox ones. */
const LINUX: System[] = ["linux"];
const ANY: System[] = ["linux", "windows", "proxmox"];

export const CATALOGUE: CatalogueEntry[] = [
  // What every server runs already: found by a search, never added twice.
  always({ id: "cpu", title: "CPU usage", description: "Share of the processors in use, and the load on Linux.", purposes: ["performance"], systems: ANY, keywords: ["processor", "load"] }),
  always({ id: "memory", title: "Memory", description: "Available memory and swap.", purposes: ["performance"], systems: ANY, keywords: ["ram", "swap", "page file"] }),
  always({ id: "disk", title: "Disk space", description: "Space and inodes used on each disk.", purposes: ["disk"], systems: ANY, keywords: ["free space", "full", "filesystem", "mount"] }),
  always({ id: "diskio", title: "Disk activity", description: "How busy and how slow each disk is.", purposes: ["disk", "performance"], systems: ANY, keywords: ["io", "latency", "wait"] }),

  // The kinds themselves, for anything the templates do not name.
  { id: "port", title: CHECK_TYPES[0]!.label, description: "Connects to a TCP port on this server or another machine.", purposes: ["availability", "network"], systems: ANY, technology: "port", keywords: ["tcp", "reachable", "connect"], type: kind("port") },
  { id: "service", title: kind("systemd").label, description: "A systemd service, socket or timer is active.", purposes: ["services", "availability"], systems: LINUX, technology: "agent", keywords: ["systemd", "unit", "daemon"], type: kind("systemd") },
  { id: "windows-service", title: kind("windows_service").label, description: "A Windows service is running.", purposes: ["services", "availability"], systems: ["windows"], technology: "agent", keywords: ["service"], type: kind("windows_service") },
  { id: "log", title: kind("log").label, description: "Counts lines matching a pattern in a log file.", purposes: ["logs"], systems: ANY, technology: "agent", keywords: ["pattern", "regex", "grep"], type: kind("log") },
  { id: "eventlog", title: kind("eventlog").label, description: "Errors and critical events of a Windows event log.", purposes: ["logs"], systems: ["windows"], technology: "agent", keywords: ["event viewer"], type: kind("eventlog") },
  { id: "script", title: "Your own script", description: "A script installed in the agent's scripts folder, read as OK, warning or critical.", purposes: ["availability"], systems: ANY, technology: "script", keywords: ["nagios", "custom", "plugin", "powershell"], type: kind("script") },

  // Linux services.
  service({ id: "ssh-service", title: "SSH server running", description: "The SSH service is active.", purposes: ["services", "availability"], systems: LINUX, keywords: ["sshd", "openssh", "remote access"] }, "ssh", "ssh on Debian and Ubuntu, sshd on RHEL and Rocky."),
  service({ id: "cron-service", title: "Cron running", description: "Scheduled jobs keep running.", purposes: ["services"], systems: LINUX, keywords: ["crond", "scheduled", "jobs"] }, "cron", "cron on Debian and Ubuntu, crond on RHEL and Rocky."),
  service({ id: "time-service", title: "Time sync running", description: "The clock stays synchronised.", purposes: ["services", "network"], systems: LINUX, keywords: ["ntp", "chrony", "timesyncd", "clock"] }, "chrony", "chrony on most systems, systemd-timesyncd on others."),
  service({ id: "nginx-service", title: "nginx running", description: "The nginx web server is active.", purposes: ["services", "web"], systems: LINUX, keywords: ["http", "reverse proxy"] }, "nginx"),
  service({ id: "apache-service", title: "Apache running", description: "The Apache web server is active.", purposes: ["services", "web"], systems: LINUX, keywords: ["httpd", "apache2", "http"] }, "apache2", "apache2 on Debian and Ubuntu, httpd on RHEL and Rocky."),
  service({ id: "postgresql-service", title: "PostgreSQL running", description: "The PostgreSQL database server is active.", purposes: ["services", "databases"], systems: LINUX, keywords: ["postgres", "sql", "database"] }, "postgresql"),
  service({ id: "mysql-service", title: "MySQL or MariaDB running", description: "The MySQL or MariaDB database server is active.", purposes: ["services", "databases"], systems: LINUX, keywords: ["mariadb", "sql", "database"] }, "mariadb", "mariadb, mysql, or mysqld on RHEL and Rocky."),
  service({ id: "redis-service", title: "Redis running", description: "The Redis server is active.", purposes: ["services", "databases"], systems: LINUX, keywords: ["cache", "key value"] }, "redis-server", "redis-server on Debian and Ubuntu, redis on RHEL and Rocky."),
  service({ id: "mongodb-service", title: "MongoDB running", description: "The MongoDB server is active.", purposes: ["services", "databases"], systems: LINUX, keywords: ["mongod", "nosql", "database"] }, "mongod"),
  service({ id: "docker-service", title: "Docker running", description: "The Docker engine is active.", purposes: ["services", "containers"], systems: LINUX, keywords: ["dockerd", "container engine"] }, "docker"),
  service({ id: "containerd-service", title: "containerd running", description: "The containerd runtime is active.", purposes: ["services", "containers"], systems: LINUX, keywords: ["kubernetes", "runtime"] }, "containerd"),
  service({ id: "postfix-service", title: "Postfix running", description: "The Postfix mail server is active.", purposes: ["services"], systems: LINUX, keywords: ["mail", "smtp", "mta"] }, "postfix"),
  service({ id: "fail2ban-service", title: "fail2ban running", description: "fail2ban keeps banning abusive addresses.", purposes: ["services", "network"], systems: LINUX, keywords: ["security", "ban", "brute force"] }, "fail2ban"),
  service({ id: "pve-cluster-service", title: "Proxmox cluster service running", description: "pve-cluster, which holds the cluster's configuration, is active.", purposes: ["services", "availability"], systems: ["proxmox"], keywords: ["pve", "cluster", "pmxcfs"] }, "pve-cluster"),
  service({ id: "pveproxy-service", title: "Proxmox web interface running", description: "pveproxy, the Proxmox web interface and API, is active.", purposes: ["services", "web"], systems: ["proxmox"], keywords: ["pve", "api", "gui"] }, "pveproxy"),

  // Windows services.
  winService({ id: "iis-service", title: "IIS running", description: "The IIS web server is running.", purposes: ["services", "web"], systems: ["windows"], keywords: ["w3svc", "http", "web server"] }, "W3SVC"),
  winService({ id: "mssql-service", title: "SQL Server running", description: "The default SQL Server instance is running.", purposes: ["services", "databases"], systems: ["windows"], keywords: ["mssql", "database", "sql"] }, "MSSQLSERVER"),
  winService({ id: "ad-service", title: "Active Directory running", description: "Active Directory Domain Services are running.", purposes: ["services", "availability"], systems: ["windows"], keywords: ["ntds", "domain controller", "ldap"] }, "NTDS"),
  winService({ id: "dns-service", title: "DNS Server running", description: "The Windows DNS Server service is running.", purposes: ["services", "network"], systems: ["windows"], keywords: ["dns"] }, "DNS"),
  winService({ id: "w32time-service", title: "Windows Time running", description: "The clock stays synchronised.", purposes: ["services", "network"], systems: ["windows"], keywords: ["ntp", "time", "clock"] }, "W32Time"),
  winService({ id: "wuauserv-service", title: "Windows Update running", description: "The Windows Update service can run.", purposes: ["services", "updates"], systems: ["windows"], keywords: ["wuauserv", "patches"] }, "wuauserv"),
  winService({ id: "spooler-service", title: "Print Spooler running", description: "The print spooler of a print server is running.", purposes: ["services"], systems: ["windows"], keywords: ["printer", "print server"] }, "Spooler"),

  // Ports.
  port({ id: "ssh-port", title: "SSH answers", description: "Port 22 accepts connections.", purposes: ["availability", "network"], systems: LINUX, keywords: ["ssh", "remote access"] }, 22),
  port({ id: "http-port", title: "Web server answers (HTTP)", description: "Port 80 accepts connections.", purposes: ["availability", "web"], systems: ANY, keywords: ["http", "website"] }, 80),
  port({ id: "https-port", title: "Web server answers (HTTPS)", description: "Port 443 accepts connections.", purposes: ["availability", "web"], systems: ANY, keywords: ["https", "tls", "ssl", "website"] }, 443),
  port({ id: "postgresql-port", title: "PostgreSQL answers", description: "Port 5432 accepts connections.", purposes: ["availability", "databases"], systems: LINUX, keywords: ["postgres", "database"] }, 5432),
  port({ id: "mysql-port", title: "MySQL or MariaDB answers", description: "Port 3306 accepts connections.", purposes: ["availability", "databases"], systems: ANY, keywords: ["mariadb", "database"] }, 3306),
  port({ id: "mssql-port", title: "SQL Server answers", description: "Port 1433 accepts connections.", purposes: ["availability", "databases"], systems: ["windows"], keywords: ["mssql", "database"] }, 1433),
  port({ id: "redis-port", title: "Redis answers", description: "Port 6379 accepts connections.", purposes: ["availability", "databases"], systems: LINUX, keywords: ["cache"] }, 6379),
  port({ id: "mongodb-port", title: "MongoDB answers", description: "Port 27017 accepts connections.", purposes: ["availability", "databases"], systems: LINUX, keywords: ["mongod", "database"] }, 27017),
  port({ id: "smtp-port", title: "Mail server answers (SMTP)", description: "Port 25 accepts connections.", purposes: ["availability", "network"], systems: ANY, keywords: ["mail", "smtp", "postfix", "exchange"] }, 25),
  port({ id: "dns-port", title: "DNS server answers", description: "Port 53 accepts TCP connections.", purposes: ["availability", "network"], systems: ANY, keywords: ["dns", "bind", "named"] }, 53),
  port({ id: "ldap-port", title: "LDAP answers", description: "Port 389 accepts connections.", purposes: ["availability", "network"], systems: ANY, keywords: ["directory", "active directory"] }, 389),
  port({ id: "smb-port", title: "File sharing answers (SMB)", description: "Port 445 accepts connections.", purposes: ["availability", "network"], systems: ANY, keywords: ["smb", "cifs", "samba", "file server", "share"] }, 445),
  port({ id: "nfs-port", title: "NFS answers", description: "Port 2049 accepts connections.", purposes: ["availability", "network", "disk"], systems: LINUX, keywords: ["nfs", "file server", "share"] }, 2049),
  port({ id: "rdp-port", title: "Remote Desktop answers", description: "Port 3389 accepts connections.", purposes: ["availability", "network"], systems: ["windows"], keywords: ["rdp", "remote access"] }, 3389),
  port({ id: "proxmox-port", title: "Proxmox web interface answers", description: "Port 8006 accepts connections.", purposes: ["availability", "web"], systems: ["proxmox"], keywords: ["pve", "api", "gui"] }, 8006),

  // Logs.
  logLines({ id: "syslog-errors", title: "Errors in syslog", description: "Lines with “error” in /var/log/syslog.", purposes: ["logs"], systems: LINUX, keywords: ["system log"] }, "/var/log/syslog", "error"),
  logLines({ id: "ssh-failures", title: "Failed SSH logins", description: "Failed password attempts in /var/log/auth.log.", purposes: ["logs", "network"], systems: LINUX, keywords: ["security", "brute force", "auth"] }, "/var/log/auth.log", "Failed password"),
  logLines({ id: "oom-kills", title: "Out of memory kills", description: "The kernel killing processes for lack of memory, in /var/log/kern.log.", purposes: ["logs", "performance"], systems: LINUX, keywords: ["oom", "kernel", "memory"] }, "/var/log/kern.log", "Out of memory|oom-kill"),
  logLines({ id: "nginx-errors", title: "nginx critical errors", description: "Critical lines of the nginx error log.", purposes: ["logs", "web"], systems: LINUX, keywords: ["http", "error log"] }, "/var/log/nginx/error.log", "\\[(crit|alert|emerg)\\]"),
  eventLog({ id: "system-events", title: "Errors in the System event log", description: "Critical and error events of the System log.", purposes: ["logs"], systems: ["windows"], keywords: ["event viewer"] }, "System"),
  eventLog({ id: "application-events", title: "Errors in the Application event log", description: "Critical and error events of the Application log.", purposes: ["logs"], systems: ["windows"], keywords: ["event viewer"] }, "Application"),

  // Scripts people already have.
  {
    id: "apt-updates",
    title: "Pending updates (check_apt)",
    description: "Warns when packages wait to be installed. Needs check_apt, from the monitoring plugins package, linked into the agent's scripts folder.",
    purposes: ["updates"],
    systems: LINUX,
    technology: "script",
    keywords: ["apt", "patches", "nagios", "security updates"],
    type: prefill(kind("script"), "Pending updates (check_apt)", { script: "check_apt" }, {
      script: "Link /usr/lib/nagios/plugins/check_apt into the agent's scripts folder first.",
    }),
  },
];

/** Shown as "Common checks" while nothing is used yet. */
export const COMMON: string[] = ["https-port", "ssh-service", "port", "service", "log", "script"];

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
  const systems = entry.systems.length === SYSTEMS.length ? "Any system" : entry.systems.map((s) => label(SYSTEMS, s)).join(", ");
  return `${systems} · ${label(TECHNOLOGIES, entry.technology)}`;
}
