import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { PageHeader, RefreshLine, StaleBanner, ErrorState } from "./PageState";
import { RelativeTime } from "./RelativeTime";
import { formatAgo, formatExact } from "./time";

const now = new Date(2026, 9, 22, 14, 32, 5);
const at = (seconds: number) => new Date(now.getTime() - seconds * 1000);

describe("formatAgo", () => {
  it("uses the short units of the handoff", () => {
    expect(formatAgo(at(12), now)).toBe("12 s ago");
    expect(formatAgo(at(41 * 60 + 30), now)).toBe("41 min ago");
    expect(formatAgo(at(5 * 3600 + 100), now)).toBe("5 h ago");
    expect(formatAgo(at(8 * 86_400), now)).toBe("8 d ago");
    expect(formatAgo(at(-90), now)).toBe("in 1 min");
  });
});

describe("RelativeTime", () => {
  it("is a <time> with the exact time in its title and accessible text", () => {
    const value = at(12);
    render(<RelativeTime value={value.toISOString()} now={now} />);
    const time = screen.getByText("12 s ago", { exact: false });
    expect(time.tagName).toBe("TIME");
    expect(time).toHaveAttribute("datetime", value.toISOString());
    expect(time).toHaveAttribute("title", "2026-10-22 14:31:53");
    expect(time).toHaveTextContent("12 s ago, 2026-10-22 14:31:53");
    expect(formatExact(value)).toBe("2026-10-22 14:31:53");
  });

  it("says when it is frozen at the last successful refresh", () => {
    render(<RelativeTime value={at(8)} now={now} stale />);
    expect(screen.getByText(/8 s ago/)).toHaveTextContent("(at 14:32:05)");
  });
});

describe("page states", () => {
  it("writes the refresh line, stale or not", () => {
    const { rerender } = render(<PageHeader title="Fleet" meta={<RefreshLine updatedAt={now} />} />);
    expect(screen.getByRole("heading", { level: 1, name: "Fleet" })).toBeInTheDocument();
    expect(screen.getByText(/Updated/)).toHaveTextContent("Updated 14:32:05, refreshes every 15 seconds");
    rerender(<PageHeader title="Fleet" meta={<RefreshLine updatedAt={now} stale />} />);
    expect(screen.getByText("Updated 14:32:05").tagName).toBe("STRONG");
    rerender(<PageHeader title="Fleet" meta={<RefreshLine updatedAt={null} />} />);
    expect(screen.getByText(/Could not load/)).toHaveTextContent("Could not load. Retrying every 15 seconds.");
  });

  it("explains a stale page and offers a retry", () => {
    render(<StaleBanner error="The server could not handle the request." updatedAt={at(15)} failures={3} onRetry={() => {}} />);
    expect(screen.getByRole("status")).toHaveTextContent(
      "Refresh failed: The server could not handle the request. Showing the last data received, from 14:31:50. 3 attempts failed. Retry now",
    );
    expect(screen.getByRole("button", { name: "Retry now" })).toBeInTheDocument();
  });

  it("shows nothing but the error on a failed first load", () => {
    render(<ErrorState what="the fleet" error="Cannot reach the MonitorFlow server." />);
    expect(screen.getByRole("alert")).toHaveTextContent("Could not load the fleet: Cannot reach the MonitorFlow server. Retrying automatically.");
    expect(screen.getByText("Nothing to show until the first successful load.")).toBeInTheDocument();
  });
});
