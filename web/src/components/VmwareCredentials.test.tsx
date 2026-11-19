import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { VmwareCredentialsRequest, VmwareTested } from "@/lib/api";
import { stubFetch } from "@/test/fetch";

import { VmwareTest, needsTrust, vmwareProblem, vmwareRequest, vmwareSummary } from "./VmwareCredentials";

const PRINT = Array.from({ length: 32 }, () => "AB").join(":");

function Harness() {
  const [value, setValue] = useState<VmwareCredentialsRequest>({ port: 443, user: "monitor@vsphere.local", password: "pw" });
  return (
    <VmwareTest
      siteId="site-1"
      address="vcenter.example.test"
      value={value}
      onTrust={(fingerprint) => setValue({ ...value, fingerprint: fingerprint ?? undefined })}
    />
  );
}

const answer = (trusted: boolean, login: boolean | null): VmwareTested => ({
  answers: true,
  error: null,
  product: "VMware vCenter Server 8.0.2",
  version: "8.0.2",
  api_type: "VirtualCenter",
  certificate: { fingerprint: PRINT, trusted, by_roots: false, subject: "vcenter.example.test", not_after: null },
  login,
  objects: login ? { vm: 412, host: 12, datastore: 9 } : ({} as Record<string, number>),
  probe: "central",
});

describe("VmwareCredentials", () => {
  beforeEach(() => vi.unstubAllGlobals());

  it("builds requests and summaries", () => {
    expect(vmwareRequest({ port: 443, user: " u ", password: "" })).toEqual({ port: 443, user: "u" });
    expect(vmwareProblem({ user: "u" }, false)).toBeTruthy();
    expect(vmwareProblem({ user: "u" }, true)).toBeNull();
    expect(vmwareSummary(null)).toBe("Not set");
    expect(vmwareSummary({ port: 443, user: "u", fingerprint: PRINT, password_set: true, updated_at: "" })).toMatch(/fingerprint/);
    expect(needsTrust(answer(false, null), {})).toBe(true);
    expect(needsTrust(answer(false, null), { fingerprint: PRINT })).toBe(false);
  });

  it("shows the certificate, and logs in once it is trusted", async () => {
    const fetchMock = stubFetch({
      "POST /api/devices/vmware/test": [{ status: 200, body: answer(false, null) }, { status: 200, body: answer(true, true) }],
    });
    render(<Harness />);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Test the connection" }));
    expect(await screen.findByText(PRINT)).toBeTruthy();
    expect(screen.queryByText(/Signed in/)).toBeNull();
    await user.click(screen.getByLabelText(/trust this certificate/));
    await user.click(screen.getByRole("button", { name: "Test the connection" }));
    expect(await screen.findByText(/Signed in: 412 virtual machines, 12 hosts, 9 datastores/)).toBeTruthy();
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(2));
    const second = JSON.parse(String(fetchMock.mock.calls[1]?.[1]?.body));
    expect(second.vmware).toEqual({ port: 443, user: "monitor@vsphere.local", password: "pw", fingerprint: PRINT });
    expect(second.site_id).toBe("site-1");
  });
});
