import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { DesktopFrame, RemoteRecording } from "@/lib/api";
import { stubFetch } from "@/test/fetch";

import { DesktopRecordingView } from "./DesktopRecording";

const whole: DesktopFrame = {
  seq: 0,
  at_ms: 0,
  x: 0,
  y: 0,
  width: 800,
  height: 600,
  screen_width: 800,
  screen_height: 600,
  bytes: 1000,
};

function recording(frames: DesktopFrame[]): RemoteRecording {
  return {
    session: {
      id: "d1",
      server_id: "web",
      hostname: "web-1",
      kind: "desktop",
      requested_by: "bob",
      created_at: "2026-11-23T09:30:00Z",
      opened_at: "2026-11-23T09:30:02Z",
      closed_at: "2026-11-23T09:41:15Z",
      close_reason: "closed by bob",
      duration_seconds: 673,
      recorded: true,
      bytes_in: 10,
      bytes_out: 100,
    },
    chunks: [],
    frames,
  };
}

beforeEach(() => {
  setActiveLocale("en");
  vi.unstubAllGlobals();
  vi.stubGlobal("createImageBitmap", async () => ({}));
});

describe("DesktopRecordingView", () => {
  it("plays the pictures, with a position and a speed on one line", async () => {
    const fetchMock = stubFetch({
      "GET /api/remote-sessions/d1/recording": { status: 200, body: recording([whole, { ...whole, seq: 1, at_ms: 1000 }]) },
      "GET /api/remote-sessions/d1/recording/frames/0": { status: 200, body: "png" },
      "GET /api/remote-sessions/d1/recording/frames/1": { status: 200, body: "png" },
    });
    const onClose = vi.fn();
    render(<DesktopRecordingView sessionId="d1" onClose={onClose} />);
    expect(await screen.findByText("Recording of the screen")).toBeInTheDocument();
    expect(screen.getByText("0:00 of 11:13")).toBeInTheDocument();
    expect(screen.getByLabelText("Recording of a remote desktop")).toBeInTheDocument();
    expect(screen.getByLabelText("Speed")).toHaveValue("1");
    await userEvent.click(screen.getByRole("button", { name: "Play" }));
    expect(screen.getByRole("button", { name: "Pause" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Close the recording" }));
    expect(onClose).toHaveBeenCalled();
    expect(fetchMock.mock.calls.map((c) => String(c[0]))).toContain("/api/remote-sessions/d1/recording");
  });

  it("says when nothing was recorded, and why it cannot load", async () => {
    stubFetch({ "GET /api/remote-sessions/d1/recording": { status: 200, body: recording([]) } });
    const { unmount } = render(<DesktopRecordingView sessionId="d1" onClose={() => {}} />);
    expect(await screen.findByText("Nothing of this screen was recorded.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Play" })).not.toBeInTheDocument();
    unmount();
    stubFetch({ "GET /api/remote-sessions/d1/recording": { status: 403, body: { error: "forbidden" } } });
    render(<DesktopRecordingView sessionId="d1" onClose={() => {}} />);
    expect(await screen.findByText(/You do not have access to this/)).toBeInTheDocument();
  });
});
