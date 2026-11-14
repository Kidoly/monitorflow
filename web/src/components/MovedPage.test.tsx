import { render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { LEGACY_SETTINGS_PATHS, movedSettingsHref } from "@/lib/routes";
import { router, setUrl } from "@/test/navigation";

import { MovedPage } from "./MovedPage";

vi.mock("next/navigation", () => import("@/test/navigation"));

describe("MovedPage", () => {
  it("replaces an old settings address with the new one, query and hash kept", () => {
    setUrl("/admin/relays/view/?id=r1#servers");
    render(<MovedPage from="/admin/relays/view/" />);
    expect(router.replace).toHaveBeenLastCalledWith("/settings/relays/view/?id=r1#servers");
    expect(window.location.pathname).toBe("/settings/relays/view/");
  });

  it("knows every page that moved, without a slash too", () => {
    expect(Object.keys(LEGACY_SETTINGS_PATHS)).toHaveLength(16);
    expect(movedSettingsHref("/admin/tokens")).toBe("/settings/add-servers/");
    expect(movedSettingsHref("/admin/settings/", "?x=1")).toBe("/settings/general/?x=1");
    expect(movedSettingsHref("/admin/mirrors/")).toBeNull();
    for (const to of Object.values(LEGACY_SETTINGS_PATHS)) expect(to).toMatch(/^\/settings\/[a-z-]+\/([a-z]+\/)?$/);
  });
});
