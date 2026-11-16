import { describe, expect, it } from "vitest";

import { asset, BASE_PATH } from "./basePath";

describe("asset", () => {
  it("leaves public files at the root in a normal build", () => {
    expect(BASE_PATH).toBe("");
    expect(asset("/brand/slime-violette.svg")).toBe("/brand/slime-violette.svg");
  });
});
