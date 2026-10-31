import { describe, expect, it } from "vitest";

import type { AgentInstall } from "./api";
import {
  type Command,
  type InstallTarget,
  allowCommandsCommand,
  endpoints,
  hostPort,
  isHttpsOrigin,
  isSafeUrl,
  isServerAddress,
  isMirrorUrl,
  linuxInstallCommand,
  relayInstallCommand,
  uninstallCommand,
  windowsInstallCommand,
} from "./install";

const SECRET = "mfe1.c2VjcmV0.Y2E";
const GITHUB = "https://github.com/Kidoly/monitorflow/releases/latest/download";
const ORIGIN = "https://mf.example";
const SUMS = "a".repeat(64);
const INSTALLER = "b".repeat(64);
const MSI = "C".repeat(64);

const install = (overrides: Partial<AgentInstall> = {}): AgentInstall => ({
  source: "github",
  custom_url: "https://files.example/mf/0.2.0/",
  source_from_config: false,
  custom_url_from_config: false,
  github_url: GITHUB,
  central: { version: "0.3.0", path: "/agent/0.3.0", sums_sha256: SUMS, installer_sha256: INSTALLER, msi_sha256: MSI },
  ...overrides,
});

const target = (overrides: Partial<InstallTarget> = {}): InstallTarget => ({
  token: SECRET,
  upstream_url: "https://mf.example:7443",
  enroll_url: "https://mf.example:7445",
  install: install(),
  ...overrides,
});

function text(command: Command): string {
  if (!("text" in command)) throw new Error(command.unavailable);
  return command.text;
}

function unavailable(command: Command): string {
  if (!("unavailable" in command)) throw new Error(`a command: ${command.text}`);
  return command.unavailable;
}

describe("install commands", () => {
  it("reads host:port from URLs, IPv6 in brackets", () => {
    expect(hostPort("https://mf.example:7443")).toBe("mf.example:7443");
    expect(hostPort("https://[2001:db8::1]:7445/")).toBe("[2001:db8::1]:7445");
    expect(hostPort("https://mf.example")).toBe("mf.example:443");
    expect(hostPort("http://mf.example:7443")).toBeNull();
  });

  it("gives the GitHub one-liner for a direct connection, without MF_ENROLL", () => {
    expect(text(linuxInstallCommand(target(), "github", ORIGIN))).toBe(
      `curl -fsSL ${GITHUB}/install.sh \\\n  | sudo MF_SERVER=mf.example:7443 MF_TOKEN=${SECRET} sh`,
    );
  });

  it("adds MF_ENROLL when the enrollment listener is elsewhere, and MF_DOWNLOAD_URL from a custom URL", () => {
    const command = linuxInstallCommand(target({ enroll_url: "https://mf.example:17445" }), "custom", ORIGIN);
    expect(text(command)).toBe(
      "curl -fsSL https://files.example/mf/0.2.0/install.sh \\\n" +
        `  | sudo MF_DOWNLOAD_URL=https://files.example/mf/0.2.0 MF_SERVER=mf.example:7443 MF_ENROLL=mf.example:17445 MF_TOKEN=${SECRET} sh`,
    );
  });

  it("pins the checksums of the central server's release instead of its certificate", () => {
    expect(text(linuxInstallCommand(target(), "central", ORIGIN))).toBe(
      `f=$(mktemp) && curl -fsSLk -o "$f" ${ORIGIN}/agent/0.3.0/install.sh \\\n` +
        `  && echo "${INSTALLER}  $f" | sha256sum -c \\\n` +
        `  && sudo MF_DOWNLOAD_URL=${ORIGIN}/agent/0.3.0 MF_SUMS_SHA256=${SUMS} MF_SERVER=mf.example:7443 MF_TOKEN=${SECRET} sh "$f"; rm -f "$f"`,
    );
    expect(text(windowsInstallCommand(target(), "central", "https://[2001:db8::1]:8443"))).toBe(
      "curl.exe -fsSLk -o monitorflow-agent.msi https://[2001:db8::1]:8443/agent/0.3.0/monitorflow-agent-windows-x86_64.msi\n" +
        `if ((Get-FileHash monitorflow-agent.msi).Hash -eq '${MSI}') {\n` +
        "  msiexec /i monitorflow-agent.msi /qn /l*v install.log `\n" +
        `    UPSTREAM_URL=https://mf.example:7443 ENROLLMENT_TOKEN=${SECRET}\n` +
        "} else { Write-Error 'monitorflow-agent.msi does not match release 0.3.0; nothing was installed' }",
    );
  });

  it("says why a source cannot be used", () => {
    const empty = target({ install: install({ central: null, custom_url: null }) });
    expect(unavailable(linuxInstallCommand(empty, "central", ORIGIN))).toMatch(/holds no agent release yet/);
    expect(unavailable(windowsInstallCommand(empty, "central", ORIGIN))).toContain("Server update");
    expect(unavailable(linuxInstallCommand(empty, "custom", ORIGIN))).toMatch(/No custom URL/);
    expect(unavailable(linuxInstallCommand(target(), "central", "http://localhost:3000"))).toMatch(/HTTPS/);
    const partial = target({
      install: install({
        central: { version: "0.3.0", path: "/agent/0.3.0", sums_sha256: SUMS, installer_sha256: null, msi_sha256: null },
      }),
    });
    expect(unavailable(linuxInstallCommand(partial, "central", ORIGIN))).toMatch(/no install.sh/);
    expect(unavailable(windowsInstallCommand(partial, "central", ORIGIN))).toMatch(/no MSI/);
    const hostile = target({ install: install({ custom_url: "https://files.example/$(id)" }) });
    expect(unavailable(linuxInstallCommand(hostile, "custom", ORIGIN))).toMatch(/shell/);
  });

  it("connects through a relay and enrolls with the server", () => {
    expect(endpoints(target(), "relay1.paris.example")).toEqual({
      server: "relay1.paris.example:7443",
      enroll: "mf.example:7445",
    });
    expect(endpoints(target(), "[2001:db8::5]:7443")).toEqual({
      server: "[2001:db8::5]:7443",
      enroll: "mf.example:7445",
    });
    expect(endpoints(target({ upstream_url: "https://[2001:db8::1]:7443", enroll_url: "https://[2001:db8::1]:7445" })))
      .toEqual({ server: "[2001:db8::1]:7443", enroll: null });
    for (const source of ["github", "central", "custom"] as const) {
      expect(text(linuxInstallCommand(target(), source, ORIGIN, "relay1:7443"))).toContain(
        `MF_SERVER=relay1:7443 MF_ENROLL=mf.example:7445 MF_TOKEN=${SECRET}`,
      );
    }
  });

  it("gives the MSI line with its properties", () => {
    expect(text(windowsInstallCommand(target(), "github", ORIGIN))).toBe(
      `Invoke-WebRequest ${GITHUB}/monitorflow-agent-windows-x86_64.msi -OutFile monitorflow-agent.msi\n` +
        "msiexec /i monitorflow-agent.msi /qn /l*v install.log `\n" +
        `  UPSTREAM_URL=https://mf.example:7443 ENROLLMENT_TOKEN=${SECRET}`,
    );
    expect(text(windowsInstallCommand(target(), "custom", ORIGIN, "relay1:7443"))).toContain(
      "Invoke-WebRequest https://files.example/mf/0.2.0/monitorflow-agent-windows-x86_64.msi",
    );
    expect(text(windowsInstallCommand(target(), "github", ORIGIN, "relay1:7443"))).toContain(
      `UPSTREAM_URL=https://relay1:7443 ENROLL_URL=https://mf.example:7445 ENROLLMENT_TOKEN=${SECRET}`,
    );
  });

  it("accepts only addresses and URLs that are safe in a shell command", () => {
    for (const ok of ["relay1", "relay1.paris.example:7443", "10.0.0.5", "[2001:db8::5]:7443"]) {
      expect(isServerAddress(ok), ok).toBe(true);
    }
    for (const bad of ["relay1; rm -rf /", "relay 1", "$(id)", "2001:db8::5", "https://relay1:7443"]) {
      expect(isServerAddress(bad), bad).toBe(false);
    }
    expect(isHttpsOrigin("https://mf.example:8443")).toBe(true);
    expect(isHttpsOrigin("https://[2001:db8::1]")).toBe(true);
    for (const bad of ["http://mf.example", "https://mf.example/x", "https://a;b"]) {
      expect(isHttpsOrigin(bad), bad).toBe(false);
    }
    expect(isSafeUrl("https://files.example/mf/0.2.0")).toBe(true);
    for (const bad of ["http://files.example", "https://files.example/`id`", "https://f.example/a b", "https://f/'"]) {
      expect(isSafeUrl(bad), bad).toBe(false);
    }
  });

  it("gives the relay's one-liner from each source, with the mirrors when asked", () => {
    expect(text(relayInstallCommand(target(), "github", ORIGIN))).toBe(
      `curl -fsSL ${GITHUB}/install.sh \\\n  | sudo MF_ROLE=relay MF_SERVER=mf.example:7443 MF_TOKEN=${SECRET} sh`,
    );
    expect(text(relayInstallCommand(target(), "custom", ORIGIN, "http://relay1.example:7480"))).toBe(
      `curl -fsSL https://files.example/mf/0.2.0/install.sh \\\n  | sudo MF_ROLE=relay MF_MIRROR_URL=http://relay1.example:7480 MF_DOWNLOAD_URL=https://files.example/mf/0.2.0 MF_SERVER=mf.example:7443 MF_TOKEN=${SECRET} sh`,
    );
    const central = text(relayInstallCommand(target(), "central", ORIGIN));
    expect(central).toContain(`echo "${INSTALLER}  $f" | sha256sum -c`);
    expect(central).toContain(`sudo MF_ROLE=relay MF_DOWNLOAD_URL=${ORIGIN}/agent/0.3.0 MF_SUMS_SHA256=${SUMS} MF_SERVER=`);
    // Published ports, as the server reports them, and a separate enrollment host.
    const published = target({ upstream_url: "https://mf.example:27443", enroll_url: "https://mf.example:27445" });
    expect(text(relayInstallCommand(published, "github", ORIGIN))).toContain(
      "MF_SERVER=mf.example:27443 MF_ENROLL=mf.example:27445",
    );
    expect(unavailable(relayInstallCommand(target({ install: install({ central: null }) }), "central", ORIGIN))).toContain(
      "Server update",
    );
    expect(isMirrorUrl("http://relay1.example:7480")).toBe(true);
    expect(isMirrorUrl("https://[2001:db8::1]:7480/")).toBe(true);
    for (const bad of ["ftp://x", "http://a b", "http://x?y", "http://x;rm"]) expect(isMirrorUrl(bad)).toBe(false);
  });
});

describe("allowCommandsCommand", () => {
  const address = {
    address: "monitor.lan",
    default: "mf.example",
    names: ["mf.example"],
    certified: false,
    upstream_url: "https://monitor.lan:7443",
    enroll_url: "https://monitor.lan:7445",
  };

  it("re-runs the installer of the default source with the opt-in", () => {
    expect(text(allowCommandsCommand(install(), address, ORIGIN))).toBe(
      `curl -fsSL ${GITHUB}/install.sh \\\n  | sudo MF_ALLOW_COMMANDS=1 sh`,
    );
    expect(text(allowCommandsCommand(install({ source: "custom" }), address, ORIGIN))).toBe(
      "curl -fsSL https://files.example/mf/0.2.0/install.sh \\\n  | sudo MF_DOWNLOAD_URL=https://files.example/mf/0.2.0 MF_ALLOW_COMMANDS=1 sh",
    );
  });

  it("pins the checksums of this server's installer, at the public address", () => {
    const command = text(allowCommandsCommand(install({ source: "central" }), address, "https://mf.example:8443"));
    expect(command).toContain("curl -fsSLk -o \"$f\" https://monitor.lan:8443/agent/0.3.0/install.sh");
    expect(command).toContain(`echo "${INSTALLER}  $f" | sha256sum -c`);
    expect(command).toContain(
      `sudo MF_DOWNLOAD_URL=https://monitor.lan:8443/agent/0.3.0 MF_SUMS_SHA256=${SUMS} MF_ALLOW_COMMANDS=1 sh "$f"`,
    );
    expect(text(allowCommandsCommand(install({ source: "central" }), { ...address, address: "2001:db8::1" }, ORIGIN))).toContain(
      "https://[2001:db8::1]/agent/0.3.0/install.sh",
    );
  });

  it("says when this server cannot offer it", () => {
    expect(unavailable(allowCommandsCommand(install({ source: "central", central: null }), address, ORIGIN))).toContain(
      "no agent release",
    );
    const noInstaller = { ...install().central!, installer_sha256: null };
    expect(unavailable(allowCommandsCommand(install({ source: "central", central: noInstaller }), address, ORIGIN))).toContain(
      "no install.sh",
    );
  });
});

describe("uninstall commands", () => {
  const address = {
    address: "monitor.lan",
    default: "mf.example",
    names: ["mf.example"],
    certified: false,
    upstream_url: "https://monitor.lan:7443",
    enroll_url: "https://monitor.lan:7445",
  };

  it("runs the installer of the default source, purging the identity", () => {
    expect(uninstallCommand("agent", "debian", install(), address, ORIGIN)).toBe(
      `curl -fsSL ${GITHUB}/install.sh \\\n  | sudo MF_UNINSTALL=1 MF_PURGE=1 sh`,
    );
    expect(uninstallCommand("relay", "linux", install({ source: "custom" }), address, ORIGIN)).toBe(
      "curl -fsSL https://files.example/mf/0.2.0/install.sh \\\n  | sudo MF_ROLE=relay MF_UNINSTALL=1 MF_PURGE=1 sh",
    );
  });

  it("pins the checksum of this server's installer, at the public address", () => {
    const command = uninstallCommand("agent", "rhel", install({ source: "central" }), address, "https://mf.example:8443");
    expect(command).toContain("curl -fsSLk -o \"$f\" https://monitor.lan:8443/agent/0.3.0/install.sh");
    expect(command).toContain(`echo "${INSTALLER}  $f" | sha256sum -c`);
    expect(command).toContain("sudo MF_UNINSTALL=1 MF_PURGE=1 sh \"$f\"");
    const v6 = { ...address, address: "2001:db8::1" };
    expect(uninstallCommand("agent", "rhel", install({ source: "central" }), v6, ORIGIN)).toContain(
      "https://[2001:db8::1]/agent/0.3.0/install.sh",
    );
  });

  it("falls back to GitHub when the default source has no installer", () => {
    const github = `curl -fsSL ${GITHUB}/install.sh`;
    for (const source of [
      install({ source: "central", central: null }),
      install({ source: "central", central: { ...install().central!, installer_sha256: null } }),
      install({ source: "custom", custom_url: null }),
    ]) {
      expect(uninstallCommand("agent", "debian", source, address, ORIGIN)).toContain(github);
    }
  });

  it("finds the MSI by its product code on Windows", () => {
    const command = uninstallCommand("agent", "windows", install(), address, ORIGIN);
    expect(command).toContain("Where-Object DisplayName -eq 'MonitorFlow agent'");
    expect(command).toContain('"/x $($app.PSChildName) /qn REMOVE_DATA=1"');
    expect(command).not.toContain("curl");
  });
});
