import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { Button, ButtonLink, buttonClass } from "./Button";
import styles from "./Button.module.css";

describe("Button", () => {
  it("is a plain button by default", () => {
    render(<Button>Refresh</Button>);
    const button = screen.getByRole("button", { name: "Refresh" });
    expect(button).toHaveAttribute("type", "button");
    expect(button).toHaveClass(styles.btn!);
    expect(button).not.toHaveClass(styles.k!);
  });

  it("draws the black and danger variants", () => {
    render(
      <>
        <Button variant="k">Enroll agent</Button>
        <Button variant="danger" disabled>
          Revoke web-01
        </Button>
      </>,
    );
    expect(screen.getByRole("button", { name: "Enroll agent" })).toHaveClass(styles.k!);
    expect(screen.getByRole("button", { name: "Revoke web-01" })).toHaveClass(styles.danger!);
    expect(screen.getByRole("button", { name: "Revoke web-01" })).toBeDisabled();
  });

  it("draws the black button on a link", () => {
    render(
      <ButtonLink variant="k" href="/admin/tokens/">
        Create an enrollment token
      </ButtonLink>,
    );
    const link = screen.getByRole("link", { name: "Create an enrollment token" });
    expect(link).toHaveClass(styles.btn!, styles.k!);
  });

  it("marks an active filter", () => {
    expect(buttonClass({ active: true }).split(" ")).toEqual([styles.btn, styles.active]);
  });
});
