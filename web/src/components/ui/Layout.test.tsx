import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it } from "vitest";

import { Breadcrumb } from "./Breadcrumb";
import { FactsBox } from "./FactsBox";
import { TabPanel, Tabs } from "./Tabs";

describe("FactsBox", () => {
  it("is a plain description list, titled when asked", () => {
    render(
      <FactsBox
        title="Threshold when it opened"
        columns={2}
        facts={[
          { label: "Site", value: "paris-dc1 · Acme Hosting" },
          { label: "Certificate ID", value: "pk_8f3a", mono: true },
        ]}
      />,
    );
    const section = screen.getByRole("region", { name: "Threshold when it opened" });
    expect(within(section).getByText("Site").tagName).toBe("DT");
    expect(within(section).getByText("pk_8f3a").tagName).toBe("DD");
  });
});

describe("Breadcrumb", () => {
  it("links the parents and marks the current page", () => {
    render(<Breadcrumb parents={[{ label: "Fleet", href: "/" }]} current="web-01" />);
    const nav = screen.getByRole("navigation", { name: "Breadcrumb" });
    expect(within(nav).getByRole("link", { name: "Fleet" })).toHaveAttribute("href", "/");
    expect(within(nav).getByText("web-01")).toHaveAttribute("aria-current", "page");
    expect(nav).toHaveTextContent("Fleet/web-01");
  });
});

describe("Tabs", () => {
  function Server() {
    const [tab, setTab] = useState("sensors");
    return (
      <>
        <Tabs
          label="Server sections"
          selected={tab}
          onSelect={setTab}
          tabs={[
            { id: "sensors", label: "Sensors", extra: "· 11" },
            { id: "updates", label: "Updates" },
            { id: "commands", label: "Commands" },
          ]}
        />
        <TabPanel id={tab}>{tab} panel</TabPanel>
      </>
    );
  }

  it("selects with clicks and arrow keys, with roving focus", async () => {
    const user = userEvent.setup();
    render(<Server />);
    const list = screen.getByRole("tablist", { name: "Server sections" });
    const [sensors, updates, commands] = within(list).getAllByRole("tab");
    expect(sensors).toHaveAttribute("aria-selected", "true");
    expect(sensors).toHaveTextContent("Sensors· 11");
    expect(updates).toHaveAttribute("tabindex", "-1");
    expect(screen.getByRole("tabpanel")).toHaveTextContent("sensors panel");
    expect(screen.getByRole("tabpanel")).toHaveAccessibleName(/Sensors/);

    await user.click(updates!);
    expect(updates).toHaveAttribute("aria-selected", "true");
    await user.keyboard("{ArrowRight}");
    expect(commands).toHaveFocus();
    expect(commands).toHaveAttribute("aria-selected", "true");
    await user.keyboard("{ArrowRight}");
    expect(sensors).toHaveFocus();
    await user.keyboard("{End}");
    expect(screen.getByRole("tabpanel")).toHaveTextContent("commands panel");
  });
});
