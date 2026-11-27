import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { JmxTested, SensorConfigEntry } from "@/lib/api";
import { stubFetch } from "@/test/fetch";

import { JolokiaCertificate, NewJolokiaCertificate, jmxTestRequest, needsJolokiaTrust, sameFingerprint } from "./JolokiaCertificate";

const PRINT = Array.from({ length: 32 }, () => "AB").join(":");
const OTHER = Array.from({ length: 32 }, () => "CD").join(":");

const answer = (fingerprint: string, trusted: boolean, reads: boolean | null): JmxTested => ({
  answers: true,
  error: null,
  certificate: { fingerprint, trusted, by_roots: false, subject: "jvm.example.test", not_after: null },
  reads,
  vm: reads ? "OpenJDK 64-Bit Server VM" : null,
  version: reads ? "21.0.5" : null,
  probe: "central",
});

function Harness() {
  const [values, setValues] = useState<Record<string, string>>({ port: "8778", user: "monitorflow", password: "pw", tls: "verify" });
  return (
    <JolokiaCertificate
      serverId="srv-1"
      request={jmxTestRequest(values, "central")}
      pinned={values.fingerprint}
      onTrust={(fingerprint) => setValues({ ...values, fingerprint: fingerprint ?? "" })}
    />
  );
}

const row: SensorConfigEntry = {
  id: "row-1",
  site_id: null,
  server_id: "srv-1",
  kind: "jmx",
  name: "jmx",
  instance: "",
  interval_ms: 60_000,
  timeout_ms: null,
  enabled: true,
  probe: "relay",
  probe_relay_id: "relay-1",
  params: { host: "192.0.2.40", tls: "verify", fingerprint: PRINT },
} as SensorConfigEntry;

describe("JolokiaCertificate", () => {
  beforeEach(() => vi.unstubAllGlobals());

  it("builds requests and compares fingerprints", () => {
    expect(jmxTestRequest({ host: " ", port: "8778", path: "", password: "", fingerprint: "" })).toEqual({ port: 8778 });
    expect(jmxTestRequest({ host: "192.0.2.4", user: " u ", password: "p" }, "central")).toEqual({
      probe: "central",
      host: "192.0.2.4",
      user: "u",
      password: "p",
    });
    expect(sameFingerprint(PRINT, PRINT.replace(/:/g, "").toLowerCase())).toBe(true);
    expect(sameFingerprint("", "")).toBe(false);
    expect(needsJolokiaTrust(answer(PRINT, false, null), undefined)).toBe(true);
    expect(needsJolokiaTrust(answer(PRINT, true, true), PRINT)).toBe(false);
    expect(needsJolokiaTrust(null, undefined)).toBe(false);
  });

  it("shows the certificate, and reads once it is trusted", async () => {
    const fetchMock = stubFetch({
      "POST /api/servers/srv-1/jmx/test": [
        { status: 200, body: answer(PRINT, false, null) },
        { status: 200, body: answer(PRINT, true, true) },
      ],
    });
    render(<Harness />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Test the certificate" }));
    expect(await screen.findByText(PRINT)).toBeTruthy();
    expect(screen.queryByText(/Jolokia answered/)).toBeNull();
    await user.click(screen.getByLabelText(/trust this certificate/));
    await user.click(screen.getByRole("button", { name: "Test the certificate" }));
    expect(await screen.findByText("Jolokia answered: OpenJDK 64-Bit Server VM 21.0.5.")).toBeTruthy();
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    const first = JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body));
    expect(first.fingerprint).toBeUndefined();
    const second = JSON.parse(String(fetchMock.mock.calls[1]?.[1]?.body));
    expect(second).toEqual({ probe: "central", port: 8778, user: "monitorflow", password: "pw", fingerprint: PRINT });
  });

  it("saves a new certificate in the check, the password kept", async () => {
    const fetchMock = stubFetch({
      "POST /api/servers/srv-1/jmx/test": [{ status: 200, body: answer(OTHER, false, null) }],
      "PUT /api/sensor_configs/row-1": [{ status: 200, body: row }],
    });
    const onSaved = vi.fn();
    render(<NewJolokiaCertificate serverId="srv-1" row={row} onClose={() => {}} onSaved={onSaved} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Test the certificate" }));
    expect(await screen.findByText(OTHER)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Trust this certificate" })).toBeNull();
    await user.click(screen.getByLabelText(/trust this certificate/));
    await user.click(screen.getByRole("button", { name: "Trust this certificate" }));
    await waitFor(() => expect(onSaved).toHaveBeenCalled());
    const test = JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body));
    expect(test).toEqual({ probe: "relay-1", host: "192.0.2.40", fingerprint: PRINT });
    const saved = JSON.parse(String(fetchMock.mock.calls[1]?.[1]?.body));
    expect(saved.params).toEqual({ host: "192.0.2.40", tls: "verify", fingerprint: OTHER });
    expect(saved.params.password).toBeUndefined();
    expect(saved.probe).toBe("relay-1");
  });
});
