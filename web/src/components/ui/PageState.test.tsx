import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";

import { ErrorState, NoPermission, PageHeader, RefreshLine, StaleBanner } from "./PageState";

describe("page states", () => {
  it("retries from the empty box of a failed first load", async () => {
    const retry = vi.fn();
    render(<ErrorState what="servers" error="Timed out." onRetry={retry} />);
    await userEvent.click(screen.getByRole("button", { name: "Retry now" }));
    expect(retry).toHaveBeenCalledOnce();
  });

  it("leads back from a page the roles do not open", () => {
    render(<NoPermission title="Mirrors need a global administrator" />);
    expect(screen.getByRole("link", { name: "Back to the fleet" })).toHaveAttribute("href", "/");
  });
});

describe("in French", () => {
  it("writes the page states in the active language", () => {
    setActiveLocale("fr");
    render(
      <>
        <ErrorState what="les serveurs" error="Délai dépassé." onRetry={() => {}} />
        <StaleBanner error="Délai dépassé." updatedAt={new Date(2026, 9, 22, 14, 32, 5)} failures={2} />
        <p>
          <RefreshLine updatedAt={new Date(2026, 9, 22, 14, 32, 5)} />
        </p>
      </>,
    );
    expect(screen.getByText("Impossible de charger les serveurs :")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Réessayer" })).toBeInTheDocument();
    expect(screen.getByText(/2 tentatives ont échoué\./)).toBeInTheDocument();
    expect(screen.getByText("Mis à jour à 14:32:05, actualisé toutes les 15 secondes")).toBeInTheDocument();
  });
});

describe("page header", () => {
  it("tints by status and says why under the title", () => {
    const { container } = render(<PageHeader detail title="Disk used /var" tone="bad" reason="97% used is above the critical limit of 95%" />);
    const header = container.firstElementChild!;
    expect(header.className).toMatch(/tinted/);
    expect(header.className).toMatch(/tint-bad/);
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Disk used /var");
    expect(screen.getByText("97% used is above the critical limit of 95%").tagName).toBe("P");
  });

  it("stays plain without a tone", () => {
    const { container } = render(<PageHeader title="Fleet" />);
    expect(container.firstElementChild!.className).not.toMatch(/tinted/);
    expect(container.querySelector("p")).toBeNull();
  });
});
