import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { Logo } from "./Logo";

const sources = (container: HTMLElement) => [...container.querySelectorAll("img")].map((img) => img.getAttribute("src"));

describe("Logo", () => {
  it("offers a light and a dark version, hidden from assistive technology", () => {
    const { container } = render(<Logo size={48} />);
    expect(sources(container)).toEqual(["/brand/slime-violette.svg", "/brand/slime-sombre.svg"]);
    expect(container.firstElementChild).toHaveAttribute("aria-hidden", "true");
  });

  it("uses the versions drawn for small sizes at 32 px and below", () => {
    const { container } = render(<Logo size={24} />);
    expect(sources(container)).toEqual(["/brand/slime-violette-petit.svg", "/brand/slime-sombre-petit.svg"]);
  });
});
