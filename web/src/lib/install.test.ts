import { describe, expect, it } from "vitest";

import {
  DEFAULT_AGENT_DOWNLOAD_URL,
  type InstallTarget,
  endpoints,
  hostPort,
  isServerAddress,
  linuxInstallCommand,
  windowsInstallCommand,
} from "./install";

const SECRET = "mfe1.c2VjcmV0.Y2E";

const target = (overrides: Partial<InstallTarget> = {}): InstallTarget => ({
  token: SECRET,
  upstream_url: "https://mf.example:7443",
  enroll_url: "https://mf.example:7445",
  agent_download_url: DEFAULT_AGENT_DOWNLOAD_URL,
  ...overrides,
});

describe("install commands", () => {
  it("reads host:port from URLs, IPv6 in brackets", () => {
    expect(hostPort("https://mf.example:7443")).toBe("mf.example:7443");
    expect(hostPort("https://[2001:db8::1]:7445/")).toBe("[2001:db8::1]:7445");
    expect(hostPort("https://mf.example")).toBe("mf.example:443");
    expect(hostPort("http://mf.example:7443")).toBeNull();
  });

  it("gives the Linux one-liner for a direct connection, without MF_ENROLL", () => {
    expect(linuxInstallCommand(target())).toBe(
      `curl -fsSL ${DEFAULT_AGENT_DOWNLOAD_URL}/install.sh \\\n  | sudo MF_SERVER=mf.example:7443 MF_TOKEN=${SECRET} sh`,
    );
  });

  it("adds MF_ENROLL when the enrollment listener is elsewhere, and MF_DOWNLOAD_URL when self-hosted", () => {
    const command = linuxInstallCommand(
      target({ enroll_url: "https://mf.example:17445", agent_download_url: "https://files.example/mf/0.2.0/" }),
    );
    expect(command).toBe(
      "curl -fsSL https://files.example/mf/0.2.0/install.sh \\\n" +
        `  | sudo MF_DOWNLOAD_URL=https://files.example/mf/0.2.0 MF_SERVER=mf.example:7443 MF_ENROLL=mf.example:17445 MF_TOKEN=${SECRET} sh`,
    );
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
    expect(linuxInstallCommand(target(), "relay1:7443")).toContain(
      `MF_SERVER=relay1:7443 MF_ENROLL=mf.example:7445 MF_TOKEN=${SECRET}`,
    );
  });

  it("gives the MSI line with its properties", () => {
    expect(windowsInstallCommand(target())).toBe(
      `Invoke-WebRequest ${DEFAULT_AGENT_DOWNLOAD_URL}/monitorflow-agent-windows-x86_64.msi -OutFile monitorflow-agent.msi\n` +
        "msiexec /i monitorflow-agent.msi /qn /l*v install.log `\n" +
        `  UPSTREAM_URL=https://mf.example:7443 ENROLLMENT_TOKEN=${SECRET}`,
    );
    expect(windowsInstallCommand(target(), "relay1:7443")).toContain(
      `UPSTREAM_URL=https://relay1:7443 ENROLL_URL=https://mf.example:7445 ENROLLMENT_TOKEN=${SECRET}`,
    );
  });

  it("accepts only addresses that are safe in a shell command", () => {
    for (const ok of ["relay1", "relay1.paris.example:7443", "10.0.0.5", "[2001:db8::5]:7443"]) {
      expect(isServerAddress(ok), ok).toBe(true);
    }
    for (const bad of ["relay1; rm -rf /", "relay 1", "$(id)", "2001:db8::5", "https://relay1:7443"]) {
      expect(isServerAddress(bad), bad).toBe(false);
    }
  });
});
