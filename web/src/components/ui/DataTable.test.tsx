import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { setUrl } from "@/test/navigation";

import { type Column, DataTable, PrimaryCell, parseSort, useUrlSort } from "./DataTable";
import { isHealthy, problemRank, problemsFirst } from "./problemsFirst";

vi.mock("next/navigation", () => import("@/test/navigation"));

interface Host {
  name: string;
  critical: number;
  warning: number;
  offline: boolean;
  seen: number | null;
}

const host = (name: string, extra: Partial<Host> = {}): Host => ({
  name,
  critical: 0,
  warning: 0,
  offline: false,
  seen: 10,
  ...extra,
});

const HOSTS = [
  host("web-10"),
  host("web-2", { offline: true, seen: 300 }),
  host("db-01", { critical: 1, seen: 8 }),
  host("build-01", { warning: 1, seen: null }),
];

const COLUMNS: Column<Host>[] = [
  { key: "host", header: "Host", cell: (h) => <PrimaryCell title={h.name} href={`/servers/view/?id=${h.name}`} sub="Debian 12" />, sortValue: (h) => h.name },
  { key: "seen", header: "Last seen", cell: (h) => h.seen ?? "—", sortValue: (h) => h.seen },
  { key: "site", header: "Site", cell: () => "paris-dc1" },
];

const facts = (h: Host) => ({ ...h, certificateWarning: false });
const names = () =>
  within(screen.getByRole("table"))
    .getAllByRole("row")
    .slice(1)
    .map((row) => within(row).getAllByRole("cell")[0]!.querySelector("b")?.textContent);

describe("DataTable", () => {
  beforeEach(() => setUrl("/"));

  it("orders problems first by default, then hostnames in natural order", () => {
    render(<DataTable label="Servers" columns={COLUMNS} rows={HOSTS} rowKey={(h) => h.name} defaultOrder={problemsFirst(facts)} />);
    expect(names()).toEqual(["db-01", "build-01", "web-2", "web-10"]);
  });

  it("toggles a sortable header between ascending and descending", async () => {
    const user = userEvent.setup();
    render(<DataTable label="Servers" columns={COLUMNS} rows={HOSTS} rowKey={(h) => h.name} />);
    const header = screen.getByRole("columnheader", { name: "Host" });
    expect(header).toHaveAttribute("aria-sort", "none");
    // Not sortable: no aria-sort, no button.
    expect(screen.getByRole("columnheader", { name: "Site" })).not.toHaveAttribute("aria-sort");

    await user.click(within(header).getByRole("button", { name: "Host" }));
    expect(header).toHaveAttribute("aria-sort", "ascending");
    expect(header).toHaveTextContent("Host↑");
    expect(names()).toEqual(["build-01", "db-01", "web-2", "web-10"]);

    await user.click(within(header).getByRole("button", { name: "Host" }));
    expect(header).toHaveAttribute("aria-sort", "descending");
    expect(header).toHaveTextContent("Host↓");
    expect(names()).toEqual(["web-10", "web-2", "db-01", "build-01"]);

    // Another column starts ascending and resets the first; missing values go last.
    await user.click(screen.getByRole("button", { name: "Last seen" }));
    expect(header).toHaveAttribute("aria-sort", "none");
    expect(names()).toEqual(["db-01", "web-10", "web-2", "build-01"]);
  });

  it("keeps the sort in the URL with useUrlSort", async () => {
    const user = userEvent.setup();
    setUrl("/?customer=acme&sort=-host");
    function Page() {
      const [sort, setSort] = useUrlSort();
      return <DataTable label="Servers" columns={COLUMNS} rows={HOSTS} rowKey={(h) => h.name} sort={sort} onSortChange={setSort} />;
    }
    render(<Page />);
    expect(screen.getByRole("columnheader", { name: "Host" })).toHaveAttribute("aria-sort", "descending");
    await user.click(screen.getByRole("button", { name: "Host" }));
    expect(window.location.search).toBe("?customer=acme&sort=host");
    expect(screen.getByRole("columnheader", { name: "Host" })).toHaveAttribute("aria-sort", "ascending");
    expect(parseSort("-seen")).toEqual({ key: "seen", direction: "desc" });
  });

  it("puts the sub-line under the main cell and actions at the end of the row", () => {
    render(
      <DataTable
        label="Servers"
        columns={COLUMNS}
        rows={[HOSTS[0]!]}
        rowKey={(h) => h.name}
        rowActions={(h) => <button type="button">Revoke {h.name}</button>}
        rowMuted={() => true}
        rowDetail={() => <pre>output</pre>}
      />,
    );
    const rows = within(screen.getByRole("table", { name: "Servers" })).getAllByRole("row");
    expect(within(rows[0]!).getByRole("columnheader", { name: "Actions" })).toBeInTheDocument();
    expect(within(rows[1]!).getByRole("link", { name: "web-10" })).toHaveAttribute("href", expect.stringMatching(/^\/servers\/view\/?\?id=web-10$/));
    expect(rows[1]).toHaveTextContent("web-10Debian 12");
    expect(within(rows[1]!).getByRole("button", { name: "Revoke web-10" })).toBeInTheDocument();
    expect(within(rows[2]!).getByRole("cell")).toHaveAttribute("colspan", "4");
  });

  it("shows 50 rows, then appends 50 more and focuses the first new row's link", async () => {
    const user = userEvent.setup();
    const many = Array.from({ length: 120 }, (_, i) => host(`web-${String(i).padStart(3, "0")}`));
    render(
      <DataTable
        label="Servers"
        columns={COLUMNS}
        rows={many}
        rowKey={(h) => h.name}
        summary={(shown, total) => `Showing ${shown} of ${total} servers`}
      />,
    );
    expect(names()).toHaveLength(50);
    expect(screen.getByText("Showing 50 of 120 servers")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Load more" }));
    expect(names()).toHaveLength(100);
    expect(screen.getByRole("link", { name: "web-050" })).toHaveFocus();
    await user.click(screen.getByRole("button", { name: "Load more" }));
    expect(names()).toHaveLength(120);
    expect(screen.getByRole("link", { name: "web-100" })).toHaveFocus();
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
  });

  it("asks the page for more rows when they come from the API", async () => {
    const user = userEvent.setup();
    const onLoadMore = vi.fn();
    const first = HOSTS.slice(0, 2);
    const { rerender } = render(
      <DataTable label="Incidents" columns={COLUMNS} rows={first} rowKey={(h) => h.name} hasMore onLoadMore={onLoadMore} />,
    );
    await user.click(screen.getByRole("button", { name: "Load more" }));
    expect(onLoadMore).toHaveBeenCalledTimes(1);
    rerender(<DataTable label="Incidents" columns={COLUMNS} rows={HOSTS} rowKey={(h) => h.name} hasMore={false} onLoadMore={onLoadMore} />);
    expect(screen.getByRole("link", { name: HOSTS[2]!.name })).toHaveFocus();
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
  });

  it("draws skeleton rows on the first load", () => {
    render(<DataTable label="Servers" columns={COLUMNS} rows={[]} rowKey={(h) => h.name} loading />);
    const table = screen.getByRole("table", { name: "Servers" });
    expect(table).toHaveAttribute("aria-busy", "true");
    expect(within(table).getAllByRole("row")).toHaveLength(4);
  });
});

describe("problemsFirst", () => {
  it("ranks critical, warning, offline, certificate, then healthy", () => {
    const base = { critical: 0, warning: 0, offline: false, certificateWarning: false, name: "a" };
    expect(problemRank({ ...base, critical: 1, offline: true })).toBe(0);
    expect(problemRank({ ...base, warning: 2 })).toBe(1);
    expect(problemRank({ ...base, offline: true, certificateWarning: true })).toBe(2);
    expect(problemRank({ ...base, certificateWarning: true })).toBe(3);
    expect(isHealthy(base)).toBe(true);
  });
});
