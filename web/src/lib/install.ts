/**
 * Install commands for a new agent token (ADR-0052): the Linux one-liner of
 * `deploy/install.sh` and the Windows MSI line (ADR-0050), from one of three
 * sources (ADR-0054); and the relay's one-liner, the same installer with
 * `MF_ROLE=relay` (ADR-0055).
 */

import type { AgentInstall, InstallSource } from "./api";

/** Port of the enrollment listener the agent assumes without `enroll_url`. */
const DEFAULT_ENROLL_PORT = "7445";

const MSI = "monitorflow-agent-windows-x86_64.msi";

/** The words for each source, in the order offered. */
export const SOURCE_WORDS: Record<InstallSource, string> = {
  github: "GitHub",
  central: "This server",
  custom: "Custom URL",
};

/** What the server answers when it holds no release, for the page. */
export const IMPORT_COMMAND = "monitorflow-server agent-release import --dir DIR";

export interface InstallTarget {
  token: string;
  /** `https://host:port` of the server, from the token response. */
  upstream_url: string;
  enroll_url: string;
  install: AgentInstall;
}

/** A command to show, or why there is none. */
export type Command = { text: string } | { unavailable: string };

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

/** Whether `origin` is an https origin that is safe in a shell command. */
export function isHttpsOrigin(origin: string): boolean {
  return /^https:\/\/(\[[0-9A-Fa-f:.]+\]|[A-Za-z0-9.-]+)(:\d{1,5})?$/.test(origin);
}

/** Whether `url` is an https URL with nothing a shell would interpret. */
export function isSafeUrl(url: string): boolean {
  return /^https:\/\/[A-Za-z0-9._~:/[\]%+-]+$/.test(url);
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

/** Why `central` offers nothing, or `null` when it can be used. */
export function centralUnavailable(install: AgentInstall, origin: string): string | null {
  if (!install.central) {
    return `This server holds no agent release yet. An administrator imports one on the server with \`${IMPORT_COMMAND}\`.`;
  }
  if (!isHttpsOrigin(origin)) return `Installs from this server need the web interface on HTTPS; this page is on ${origin}.`;
  return null;
}

/** Base URL of the files of `source`, or why there is none. */
function base(install: AgentInstall, source: InstallSource, origin: string): { url: string } | { unavailable: string } {
  switch (source) {
    case "github":
      return { url: install.github_url.replace(/\/+$/, "") };
    case "custom":
      if (!install.custom_url) {
        return { unavailable: "No custom URL is set. A global administrator sets one under Default install source." };
      }
      return isSafeUrl(install.custom_url)
        ? { url: install.custom_url.replace(/\/+$/, "") }
        : { unavailable: "The custom URL has characters a shell would interpret; set a plain https URL." };
    case "central": {
      const why = centralUnavailable(install, origin);
      return why || !install.central ? { unavailable: why ?? "" } : { url: `${origin}${install.central.path}` };
    }
  }
}

/**
 * The Linux command, as in docs/deploy-agent.md. `central` pins the
 * checksums of install.sh and SHA256SUMS instead of the certificate, so
 * hosts need not trust it; `origin` is this page's, where the server's files are.
 */
export function linuxInstallCommand(
  target: InstallTarget,
  source: InstallSource,
  origin: string,
  relay = "",
): Command {
  return installerCommand(target, source, origin, relay, []);
}

/**
 * The relay's command, as in docs/deploy-relay.md: the same installer with
 * `MF_ROLE=relay`, connecting to the server; `mirrorUrl` turns on its copy
 * of the mirrors.
 */
export function relayInstallCommand(
  target: InstallTarget,
  source: InstallSource,
  origin: string,
  mirrorUrl = "",
): Command {
  const mirror = mirrorUrl.trim() ? [`MF_MIRROR_URL=${mirrorUrl.trim()}`] : [];
  return installerCommand(target, source, origin, "", ["MF_ROLE=relay", ...mirror]);
}

/** Whether `url` can be a relay's mirror URL in a shell command, as the installer accepts it. */
export function isMirrorUrl(url: string): boolean {
  return /^https?:\/\/[A-Za-z0-9.:_/[\]-]+$/.test(url.trim());
}

function installerCommand(
  target: InstallTarget,
  source: InstallSource,
  origin: string,
  relay: string,
  role: string[],
): Command {
  const files = base(target.install, source, origin);
  if ("unavailable" in files) return files;
  const { server, enroll } = endpoints(target, relay);
  const env = (download: string[]) =>
    [...role, ...download, `MF_SERVER=${server}`, enroll && `MF_ENROLL=${enroll}`, `MF_TOKEN=${target.token}`]
      .filter(Boolean)
      .join(" ");
  return runInstaller(target.install, source, files.url, env);
}

/** The one line that lets an installed Linux agent run AI commands (ADR-0064), from the default source. */
export function allowCommandsCommand(install: AgentInstall, origin: string): Command {
  const files = base(install, install.source, origin);
  if ("unavailable" in files) return files;
  return runInstaller(install, install.source, files.url, (download) => [...download, "MF_ALLOW_COMMANDS=1"].join(" "));
}

/** Fetches install.sh from `url` and runs it with `env` of the download variables. */
function runInstaller(
  install: AgentInstall,
  source: InstallSource,
  url: string,
  env: (download: string[]) => string,
): Command {
  const central = install.central;
  if (source === "central" && central) {
    if (!central.installer_sha256) {
      return { unavailable: `The release ${central.version} on this server has no install.sh; import a complete release.` };
    }
    const vars = env([`MF_DOWNLOAD_URL=${url}`, `MF_SUMS_SHA256=${central.sums_sha256}`]);
    return {
      text:
        `f=$(mktemp) && curl -fsSLk -o "$f" ${url}/install.sh \\\n` +
        `  && echo "${central.installer_sha256}  $f" | sha256sum -c \\\n` +
        `  && sudo ${vars} sh "$f"; rm -f "$f"`,
    };
  }
  const vars = env(source === "custom" ? [`MF_DOWNLOAD_URL=${url}`] : []);
  return { text: `curl -fsSL ${url}/install.sh \\\n  | sudo ${vars} sh` };
}

/** Download and silent install of the MSI, for an elevated PowerShell. */
export function windowsInstallCommand(
  target: InstallTarget,
  source: InstallSource,
  origin: string,
  relay = "",
): Command {
  const files = base(target.install, source, origin);
  if ("unavailable" in files) return files;
  const { server, enroll } = endpoints(target, relay);
  const properties = [
    `UPSTREAM_URL=https://${server}`,
    enroll && `ENROLL_URL=https://${enroll}`,
    `ENROLLMENT_TOKEN=${target.token}`,
  ].filter(Boolean);
  const msiexec = `msiexec /i monitorflow-agent.msi /qn /l*v install.log \``;
  const central = target.install.central;
  if (source === "central" && central) {
    if (!central.msi_sha256) {
      return { unavailable: `The release ${central.version} on this server has no MSI; import a complete release.` };
    }
    // One block, so a mismatch stops it even when pasted line by line.
    return {
      text: [
        `curl.exe -fsSLk -o monitorflow-agent.msi ${files.url}/${MSI}`,
        `if ((Get-FileHash monitorflow-agent.msi).Hash -eq '${central.msi_sha256}') {`,
        `  ${msiexec}`,
        `    ${properties.join(" ")}`,
        `} else { Write-Error 'monitorflow-agent.msi does not match release ${central.version}; nothing was installed' }`,
      ].join("\n"),
    };
  }
  return {
    text: [
      `Invoke-WebRequest ${files.url}/${MSI} -OutFile monitorflow-agent.msi`,
      `${msiexec}\n  ${properties.join(" ")}`,
    ].join("\n"),
  };
}
