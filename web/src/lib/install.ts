/**
 * Install commands for a new agent token (ADR-0052): the Linux one-liner of
 * `deploy/install.sh` and the Windows MSI line (ADR-0050).
 */

/** The server's default `agent_download_url`: the latest GitHub release. */
export const DEFAULT_AGENT_DOWNLOAD_URL = "https://github.com/Kidoly/monitorflow/releases/latest/download";

/** Port of the enrollment listener the agent assumes without `enroll_url`. */
const DEFAULT_ENROLL_PORT = "7445";

export interface InstallTarget {
  token: string;
  /** `https://host:port` of the server, from the token response. */
  upstream_url: string;
  enroll_url: string;
  /** Base URL of the release files. */
  agent_download_url: string;
}

/** `host:port` of an https URL, IPv6 hosts kept in brackets; `null` otherwise. */
export function hostPort(url: string): string | null {
  const match = /^https:\/\/(\[[0-9A-Fa-f:.]+\]|[^/:[\]]+)(?::(\d+))?\/?$/.exec(url.trim());
  if (!match) return null;
  return `${match[1]}:${match[2] ?? "443"}`;
}

/** Whether `value` can be a `host:port` in a shell command: no spaces or quotes. */
export function isServerAddress(value: string): boolean {
  return /^(\[[0-9A-Fa-f:.]+\]|[A-Za-z0-9._-]+)(:\d{1,5})?$/.test(value.trim());
}

/** `host` or `[v6]` gets the gRPC port 7443, as in the installer. */
function withPort(server: string): string {
  if (!server) return "";
  return /^\[.*\]$/.test(server) || !server.includes(":") ? `${server}:7443` : server;
}

function host(hostAndPort: string): string {
  return hostAndPort.slice(0, hostAndPort.lastIndexOf(":"));
}

/**
 * `host:port` the agent connects to, and the enrollment listener when the
 * agent's default (that host, port 7445) is not it. `relay` is an optional
 * `host:port` of a relay to connect through instead of the server.
 */
export function endpoints(target: InstallTarget, relay = ""): { server: string; enroll: string | null } {
  const server = withPort(relay.trim()) || hostPort(target.upstream_url) || target.upstream_url;
  const enroll = hostPort(target.enroll_url) || target.enroll_url;
  return { server, enroll: enroll === `${host(server)}:${DEFAULT_ENROLL_PORT}` ? null : enroll };
}

function base(target: InstallTarget): string {
  return target.agent_download_url.replace(/\/+$/, "");
}

/** The `curl … | sudo … sh` line, as in docs/deploy-agent.md. */
export function linuxInstallCommand(target: InstallTarget, relay = ""): string {
  const { server, enroll } = endpoints(target, relay);
  const files = base(target);
  const vars = [
    files === DEFAULT_AGENT_DOWNLOAD_URL ? null : `MF_DOWNLOAD_URL=${files}`,
    `MF_SERVER=${server}`,
    enroll && `MF_ENROLL=${enroll}`,
    `MF_TOKEN=${target.token}`,
  ].filter(Boolean);
  return `curl -fsSL ${files}/install.sh \\\n  | sudo ${vars.join(" ")} sh`;
}

/** Download and silent install of the MSI, for an elevated PowerShell. */
export function windowsInstallCommand(target: InstallTarget, relay = ""): string {
  const { server, enroll } = endpoints(target, relay);
  const properties = [
    `UPSTREAM_URL=https://${server}`,
    enroll && `ENROLL_URL=https://${enroll}`,
    `ENROLLMENT_TOKEN=${target.token}`,
  ].filter(Boolean);
  return [
    `Invoke-WebRequest ${base(target)}/monitorflow-agent-windows-x86_64.msi -OutFile monitorflow-agent.msi`,
    `msiexec /i monitorflow-agent.msi /qn /l*v install.log \`\n  ${properties.join(" ")}`,
  ].join("\n");
}
