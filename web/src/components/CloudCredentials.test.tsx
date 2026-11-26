import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { stubFetch } from "@/test/fetch";

import { CloudFields, type CloudForm, CloudTest, NEW_CLOUD, cloudForm, cloudProblem, cloudRequest, cloudSummary } from "./CloudCredentials";

const AWS: CloudForm = { ...NEW_CLOUD, key_id: " AKIAIOSFODNN7EXAMPLE ", secret: "s3cret", regions: "eu-west-3, us-east-1" };
const GUID = "72f988bf-86f1-41af-91ab-2d7cd011db47";

function Harness() {
  const [value, setValue] = useState<CloudForm>(NEW_CLOUD);
  return (
    <>
      <CloudFields value={value} onChange={setValue} />
      <CloudTest siteId="site-1" value={value} />
    </>
  );
}

describe("CloudCredentials", () => {
  beforeEach(() => vi.unstubAllGlobals());

  it("builds requests and summaries", () => {
    expect(cloudRequest(AWS)).toEqual({
      provider: "aws",
      key_id: "AKIAIOSFODNN7EXAMPLE",
      secret: "s3cret",
      regions: ["eu-west-3", "us-east-1"],
    });
    const azure: CloudForm = { provider: "azure", key_id: GUID, secret: "", regions: "", tenant: GUID, subscription: GUID };
    expect(cloudRequest(azure)).toEqual({ provider: "azure", key_id: GUID, tenant: GUID, subscription: GUID });
    expect(cloudProblem(azure, false)).toBeTruthy();
    expect(cloudProblem(azure, true)).toBeNull();
    expect(cloudProblem({ ...AWS, regions: " , " }, false)).toBeTruthy();
    expect(cloudSummary(null)).toBe("Not set");
    const stored = {
      provider: "aws" as const,
      key_id: "AKIAIOSFODNN7EXAMPLE",
      regions: ["eu-west-3"],
      tenant: null,
      subscription: null,
      secret_set: true,
      updated_at: "",
    };
    expect(cloudSummary(stored)).toBe("AWS, key AKIAIOSFODNN7EXAMPLE, eu-west-3");
    expect(cloudForm(stored)).toEqual({ ...NEW_CLOUD, key_id: "AKIAIOSFODNN7EXAMPLE", regions: "eu-west-3" });
  });

  it("asks for each provider's fields, and tests them from the central server", async () => {
    const fetchMock = stubFetch({
      "POST /api/devices/cloud/test": [
        {
          status: 200,
          body: { login: true, reason: null, error: null, vms: 14, running: 11, metrics: false, warning: "AccessDenied: no" },
        },
      ],
    });
    render(<Harness />);
    const user = userEvent.setup();
    expect(screen.getByText(/ec2:DescribeInstances/)).toBeTruthy();
    await user.selectOptions(screen.getByLabelText("Provider"), "azure");
    expect(screen.getByLabelText("Directory (tenant) ID")).toBeTruthy();
    expect(screen.queryByLabelText("Regions")).toBeNull();
    await user.type(screen.getByLabelText("Directory (tenant) ID"), GUID);
    await user.type(screen.getByLabelText("Application (client) ID"), GUID);
    await user.type(screen.getByLabelText("Client secret"), "a secret");
    await user.click(screen.getByRole("button", { name: "Test the credentials" }));
    expect(await screen.findByText("Fill in every field.")).toBeTruthy();
    expect(fetchMock).not.toHaveBeenCalled();
    await user.type(screen.getByLabelText("Subscription ID"), GUID);
    await user.click(screen.getByRole("button", { name: "Test the credentials" }));
    expect(await screen.findByText("Signed in: 14 virtual machines, 11 running.")).toBeTruthy();
    expect(screen.getByText("Metrics cannot be read: AccessDenied: no")).toBeTruthy();
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    const body = JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body));
    expect(body).toEqual({
      site_id: "site-1",
      cloud: { provider: "azure", key_id: GUID, secret: "a secret", tenant: GUID, subscription: GUID },
    });
  });
});
