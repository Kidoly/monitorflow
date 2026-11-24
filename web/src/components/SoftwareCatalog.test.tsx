import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { SoftwareEntry } from "@/lib/api";
import { As, customerAdmin, me, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { SoftwareCatalog } from "./SoftwareCatalog";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = me({ role: "administrator", customer_id: null, site_id: null });
const ID = "1b7c5d6e-1f2a-4b3c-8d4e-5f6a7b8c9d01";
const shared: SoftwareEntry = {
  id: ID,
  customer_id: null,
  name: "7-Zip",
  manager: "winget",
  package_id: "7zip.7zip",
  source: "winget",
  description: "",
  created_by: "u1",
  created_at: "2026-11-20T10:00:00Z",
  updated_at: "2026-11-20T10:00:00Z",
};
const own: SoftwareEntry = { ...shared, id: "2b7c5d6e-1f2a-4b3c-8d4e-5f6a7b8c9d02", customer_id: "c1", name: "Firefox", package_id: "Mozilla.Firefox", description: "Browser" };
const customers = { status: 200, body: { customers: [{ id: "c1", name: "acme", site_count: 1 }, { id: "c2", name: "globex", site_count: 1 }] } };
const list = (software: SoftwareEntry[]) => ({ status: 200, body: { software } });

function show(user = globalAdmin) {
  return render(
    <As user={user}>
      <SoftwareCatalog />
    </As>,
  );
}

afterEach(() => setActiveLocale("en"));

describe("SoftwareCatalog", () => {
  it("lists entries with their customer, and adds a shared one", async () => {
    const fetchMock = stubFetch({
      "GET /api/software": [list([shared, own]), list([shared, own])],
      "GET /api/customers": customers,
      "POST /api/software": { status: 201, body: shared },
    });
    show();
    const table = await screen.findByRole("table", { name: "Software catalogue" });
    expect(within(table).getByText("7zip.7zip")).toBeInTheDocument();
    expect(within(table).getByText("Shared")).toBeInTheDocument();
    expect(within(table).getByText("acme")).toBeInTheDocument();
    expect(screen.getByText(/installed with winget on Windows servers/)).toBeInTheDocument();

    const form = screen.getByRole("form", { name: "Add software" });
    await userEvent.type(within(form).getByLabelText("Name"), "Notepad++");
    await userEvent.type(within(form).getByLabelText("Package ID"), "Notepad++.Notepad++");
    expect(within(form).getByLabelText("Customer")).toHaveValue("");
    await userEvent.click(within(form).getByRole("button", { name: "Add" }));
    const post = fetchMock.mock.calls.find(([, init]) => init?.method === "POST");
    expect(JSON.parse(String(post?.[1]?.body))).toEqual({
      customer_id: null,
      name: "Notepad++",
      manager: "winget",
      package_id: "Notepad++.Notepad++",
      source: "winget",
      description: "",
    });
  });

  it("checks the package ID before sending, and refuses the store", async () => {
    const fetchMock = stubFetch({ "GET /api/software": list([]), "GET /api/customers": customers });
    show();
    expect(await screen.findByText("No software in the catalogue yet.")).toBeInTheDocument();
    const form = screen.getByRole("form", { name: "Add software" });
    await userEvent.type(within(form).getByLabelText("Name"), "Thing");
    await userEvent.type(within(form).getByLabelText("Package ID"), "thing");
    await userEvent.clear(within(form).getByLabelText("Source"));
    await userEvent.type(within(form).getByLabelText("Source"), "msstore");
    await userEvent.click(within(form).getByRole("button", { name: "Add" }));
    expect(screen.getByText("Give a winget ID such as 7zip.7zip.")).toBeInTheDocument();
    expect(screen.getByText(/never msstore/)).toBeInTheDocument();
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === "POST")).toBe(false);
  });

  it("lets a customer administrator change only the customer's entries, without the shared option", async () => {
    stubFetch({
      "GET /api/software": [list([shared, own]), list([shared])],
      "GET /api/customers": customers,
      [`PUT /api/software/${own.id}`]: { status: 200, body: own },
      [`DELETE /api/software/${own.id}`]: { status: 204 },
    });
    show(customerAdmin);
    await screen.findByRole("table", { name: "Software catalogue" });
    expect(screen.queryByRole("button", { name: "Edit 7-Zip" })).not.toBeInTheDocument();
    const customer = screen.getByLabelText("Customer");
    expect(within(customer).queryByText("Shared with every customer")).not.toBeInTheDocument();
    expect(customer).toHaveValue("c1");

    await userEvent.click(screen.getByRole("button", { name: "Edit Firefox" }));
    const form = screen.getByRole("form", { name: "Edit Firefox" });
    expect(within(form).getByLabelText("Package ID")).toHaveValue("Mozilla.Firefox");
    expect(within(form).getByLabelText("Customer")).toBeDisabled();
    await userEvent.click(within(form).getByRole("button", { name: "Cancel" }));

    await userEvent.click(screen.getByRole("button", { name: "Delete Firefox" }));
    await userEvent.click(screen.getByRole("button", { name: "Delete Firefox" }));
    expect(await screen.findByText("7-Zip")).toBeInTheDocument();
    expect(screen.queryByText("Firefox")).not.toBeInTheDocument();
  });

  it("shows a failed save in a banner", async () => {
    stubFetch({
      "GET /api/software": list([]),
      "GET /api/customers": customers,
      "POST /api/software": { status: 409, body: { error: "7zip.7zip is already listed" } },
    });
    show();
    const form = await screen.findByRole("form", { name: "Add software" });
    await userEvent.type(within(form).getByLabelText("Name"), "7-Zip");
    await userEvent.type(within(form).getByLabelText("Package ID"), "7zip.7zip");
    await userEvent.click(within(form).getByRole("button", { name: "Add" }));
    expect(await screen.findByText(/Adding the software failed\./)).toBeInTheDocument();
  });

  it("reads only for a viewer, in French too", async () => {
    setActiveLocale("fr");
    stubFetch({ "GET /api/software": list([shared]), "GET /api/customers": customers });
    show(viewer);
    expect(await screen.findByRole("table", { name: "Catalogue de logiciels" })).toBeInTheDocument();
    expect(screen.getByText("Partagé")).toBeInTheDocument();
    expect(screen.queryByRole("form")).not.toBeInTheDocument();
  });
});
