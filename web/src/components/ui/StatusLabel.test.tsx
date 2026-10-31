import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { Label, STATUS_WORDS, type StatusKind, StatusLabel } from "./StatusLabel";
import styles from "./StatusLabel.module.css";

describe("StatusLabel", () => {
  it("writes every status of the handoff as a word in its tint", () => {
    for (const [kind, words] of Object.entries(STATUS_WORDS)) {
      for (const [value, [word, tone]] of Object.entries(words)) {
        const { unmount } = render(<StatusLabel kind={kind as StatusKind} value={value as never} />);
        const label = screen.getByText(word);
        expect(label).toHaveClass(styles.label!, styles[tone]!);
        unmount();
      }
    }
  });

  it("covers the README's vocabulary", () => {
    const words = new Set(Object.values(STATUS_WORDS).flatMap((w) => Object.values(w).map(([word]) => word)));
    for (const word of ["Online", "Offline", "Revoked", "OK", "Warning", "Critical", "Unknown", "Pending", "Approved", "Installed", "Failed", "Rejected", "Read-only", "Low-impact", "High-impact", "Forbidden"]) {
      expect(words).toContain(word);
    }
  });

  it("tints the same word by domain", () => {
    render(
      <>
        <StatusLabel kind="token" value="revoked" />
        <StatusLabel kind="certificate" value="revoked" />
        <StatusLabel kind="token" value="pending" />
        <StatusLabel kind="patch" value="pending" small />
      </>,
    );
    const [server, certificate] = screen.getAllByText("Revoked");
    expect(server).toHaveClass(styles.neutral!);
    expect(certificate).toHaveClass(styles.bad!);
    const [token, patch] = screen.getAllByText("Pending");
    expect(token).toHaveClass(styles.info!);
    expect(patch).toHaveClass(styles.warn!, styles.small!);
  });

  it("takes free words", () => {
    render(<Label tone="warn">Expires in 5 days</Label>);
    expect(screen.getByText("Expires in 5 days")).toHaveClass(styles.warn!);
  });
});
