import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import { CATALOGUE } from "@/lib/catalogue";

import { CheckCatalogue } from "./CheckCatalogue";

const entry = (id: string) => CATALOGUE.find((e) => e.id === id)!;

describe("CheckCatalogue", () => {
  it("shows common checks until something is used, and the most used after", () => {
    const props = { entries: CATALOGUE, systems: ["linux", "windows", "proxmox"] as const, onChoose: vi.fn(), onCancel: vi.fn() };
    const { rerender } = render(<CheckCatalogue {...props} systems={[...props.systems]} mostUsed={[]} />);
    expect(screen.getByRole("region", { name: "Common checks" })).toBeInTheDocument();
    rerender(<CheckCatalogue {...props} systems={[...props.systems]} mostUsed={[entry("postgresql-port")]} />);
    const top = screen.getByRole("region", { name: "Most used" });
    expect(within(top).getAllByRole("button")).toHaveLength(1);
    expect(within(top).getByRole("button")).toHaveTextContent("PostgreSQL answers");
  });

  it("speaks French", () => {
    setActiveLocale("fr");
    render(<CheckCatalogue entries={CATALOGUE} mostUsed={[]} systems={["linux", "proxmox"]} onChoose={vi.fn()} onCancel={vi.fn()} />);
    expect(screen.getByRole("group", { name: "Catalogue des contrôles" })).toBeInTheDocument();
    expect(screen.getByText(`${CATALOGUE.length} résultats`)).toBeInTheDocument();
    const common = screen.getByRole("region", { name: "Contrôles courants" });
    expect(within(common).getAllByRole("button")[0]).toHaveTextContent("Un site web répond");
    expect(screen.getByRole("group", { name: "Ce qu’il faut surveiller" })).toHaveTextContent(/Disponibilité/);
  });

  it("searches live, filters, and hands the chosen check on", async () => {
    const user = userEvent.setup();
    const onChoose = vi.fn();
    render(<CheckCatalogue entries={CATALOGUE} mostUsed={[]} systems={["linux", "proxmox"]} onChoose={onChoose} onCancel={vi.fn()} />);
    expect(screen.getByText(`${CATALOGUE.length} matching`)).toBeInTheDocument();
    const list = () => screen.getByRole("region", { name: /^(All|Matching) checks$/ });
    expect(within(screen.getByRole("region", { name: "All checks" })).getAllByRole("listitem")).toHaveLength(12);
    await user.click(screen.getByRole("button", { name: `Show all ${CATALOGUE.length} checks` }));
    expect(within(list()).getAllByRole("listitem")).toHaveLength(CATALOGUE.length);

    const databases = within(screen.getByRole("group", { name: "What to monitor" })).getByRole("button", { name: /^Databases/ });
    await user.click(databases);
    expect(databases).toHaveAttribute("aria-pressed", "true");
    // The common checks give way to the matches.
    expect(screen.queryByRole("region", { name: "Common checks" })).not.toBeInTheDocument();
    expect(within(list()).getAllByRole("listitem")).toHaveLength(10);
    await user.type(screen.getByLabelText("Find a check"), "redis");
    expect(screen.getByText("2 matching")).toBeInTheDocument();
    const port = within(screen.getByRole("group", { name: "How it works" })).getByRole("button", { name: /^Port/ });
    expect(port).toHaveTextContent("Port 1");
    await user.click(port);
    await user.click(screen.getByRole("button", { name: /^Redis answers/ }));
    expect(onChoose).toHaveBeenCalledWith(entry("redis-port"));

    await user.type(screen.getByLabelText("Find a check"), " nothing");
    expect(screen.getByText("0 matching")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Clear the search and filters" }));
    expect(screen.getByLabelText("Find a check")).toHaveValue("");
  });

  it("says a check every server runs is there already", async () => {
    const user = userEvent.setup();
    const onChoose = vi.fn();
    render(<CheckCatalogue entries={CATALOGUE} mostUsed={[]} systems={["linux"]} onChoose={onChoose} onCancel={vi.fn()} />);
    expect(screen.queryByRole("group", { name: "System" })).not.toBeInTheDocument();
    await user.type(screen.getByLabelText("Find a check"), "cpu");
    await user.click(screen.getByRole("button", { name: /^CPU usage/ }));
    expect(onChoose).not.toHaveBeenCalled();
    expect(screen.getByRole("status")).toHaveTextContent("CPU usage is checked on every server already.");
  });
});
