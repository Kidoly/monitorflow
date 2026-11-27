import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { stubFetch } from "@/test/fetch";

import {
  CloudFields,
  type CloudForm,
  CloudTest,
  NEW_CLOUD,
  cloudForm,
  cloudProblem,
  cloudRequest,
  cloudSummary,
  readGcpKey,
} from "./CloudCredentials";

const AWS: CloudForm = { ...NEW_CLOUD, key_id: " AKIAIOSFODNN7EXAMPLE ", secret: "s3cret", regions: "eu-west-3, us-east-1" };
const GUID = "72f988bf-86f1-41af-91ab-2d7cd011db47";
const ACCOUNT = "reader@example-project.iam.gserviceaccount.com";
/** A key's shape; the browser only reads its account and project. */
const KEY = JSON.stringify({
  type: "service_account",
  project_id: "example-project",
  private_key: "-----BEGIN PRIVATE KEY-----\nnot a real key\n-----END PRIVATE KEY-----\n",
  client_email: ACCOUNT,
});

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
    const azure: CloudForm = { ...NEW_CLOUD, provider: "azure", key_id: GUID, tenant: GUID, subscription: GUID };
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
      project: null,
      secret_set: true,
      updated_at: "",
    };
    expect(cloudSummary(stored)).toBe("AWS, key AKIAIOSFODNN7EXAMPLE, eu-west-3");
    expect(cloudForm(stored)).toEqual({ ...NEW_CLOUD, key_id: "AKIAIOSFODNN7EXAMPLE", regions: "eu-west-3" });
    const gcp = { ...stored, provider: "gcp" as const, key_id: ACCOUNT, regions: [], project: "example-project" };
    expect(cloudSummary(gcp)).toBe("Google Cloud, project example-project");
    const kept = cloudForm(gcp);
    expect(cloudProblem(kept, true)).toBeNull();
    expect(cloudProblem(kept, false)).toBeTruthy();
    expect(cloudRequest({ ...kept, project: " other-project " })).toEqual({ provider: "gcp", key_id: ACCOUNT, project: "other-project" });
    expect(readGcpKey(KEY)).toEqual({ email: ACCOUNT, project: "example-project" });
    expect(readGcpKey('{"type": "authorized_user"}')).toBeNull();
    expect(readGcpKey("not json")).toBeNull();
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

  it("reads a Google service account's key file and its project", async () => {
    const fetchMock = stubFetch({
      "POST /api/devices/cloud/test": [
        { status: 200, body: { login: true, reason: null, error: null, vms: 3, running: 2, metrics: true, warning: null } },
      ],
    });
    render(<Harness />);
    const user = userEvent.setup({ applyAccept: false });
    await user.selectOptions(screen.getByLabelText("Provider"), "gcp");
    expect(screen.getByText(/Compute Viewer and Monitoring Viewer/)).toBeTruthy();
    expect(screen.queryByLabelText("Access key ID")).toBeNull();
    const input = screen.getByLabelText("Service account key (JSON file)");
    await user.upload(input, new File(["not a key"], "key.json", { type: "application/json" }));
    expect(await screen.findByText("This file is not a service account's JSON key.")).toBeTruthy();
    await user.upload(input, new File([KEY], "key.json", { type: "application/json" }));
    expect(await screen.findByText(`Key of ${ACCOUNT}.`)).toBeTruthy();
    expect((screen.getByLabelText("Project ID") as HTMLInputElement).value).toBe("example-project");
    await user.click(screen.getByRole("button", { name: "Test the credentials" }));
    expect(await screen.findByText("Signed in: 3 virtual machines, 2 running.")).toBeTruthy();
    expect(screen.getByText("Metrics can be read.")).toBeTruthy();
    const body = JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body));
    expect(body).toEqual({
      site_id: "site-1",
      cloud: { provider: "gcp", key_id: ACCOUNT, secret: KEY, project: "example-project" },
    });
  });
});
