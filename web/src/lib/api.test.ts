import { afterEach, describe, expect, it, vi } from "vitest";

import { ApiError, CSRF_HEADER, api, describeError, setUnauthorizedHandler } from "./api";

function mockFetch(status: number, body?: unknown) {
  const fetchMock = vi.fn<typeof fetch>(
    async () =>
      new Response(body === undefined ? null : JSON.stringify(body), {
        status,
        headers: { "Content-Type": "application/json" },
      }),
  );
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

function headersOf(fetchMock: ReturnType<typeof mockFetch>): Record<string, string> {
  return fetchMock.mock.calls[0]?.[1]?.headers as Record<string, string>;
}

describe("api client", () => {
  let restore: (() => void) | undefined;
  afterEach(() => restore?.());

  it("sends the CSRF header and same-origin credentials on POST", async () => {
    const fetchMock = mockFetch(200, { id: "u1", username: "alice", roles: [] });
    const me = await api.login("alice", "secret");
    expect(me.username).toBe("alice");
    const [url, init] = fetchMock.mock.calls[0]!;
    expect(url).toBe("/api/auth/login");
    expect(init?.method).toBe("POST");
    expect(init?.credentials).toBe("same-origin");
    expect(headersOf(fetchMock)[CSRF_HEADER]).toBe("1");
    expect(JSON.parse(init?.body as string)).toEqual({ username: "alice", password: "secret" });
  });

  it("does not send the CSRF header on GET", async () => {
    const fetchMock = mockFetch(200, { servers: [] });
    await api.servers();
    expect(fetchMock.mock.calls[0]?.[1]?.credentials).toBe("same-origin");
    expect(headersOf(fetchMock)).not.toHaveProperty(CSRF_HEADER);
  });

  it("returns nothing for 204", async () => {
    mockFetch(204);
    await expect(api.logout()).resolves.toBeUndefined();
  });

  it("calls the unauthorized handler on 401", async () => {
    const handler = vi.fn();
    restore = setUnauthorizedHandler(handler);
    mockFetch(401, { error: "authentication required" });
    await expect(api.me()).rejects.toMatchObject({ status: 401, message: "authentication required" });
    expect(handler).toHaveBeenCalledOnce();
  });

  it("does not redirect when login is refused", async () => {
    const handler = vi.fn();
    restore = setUnauthorizedHandler(handler);
    mockFetch(401, { error: "invalid credentials" });
    await expect(api.login("alice", "wrong")).rejects.toMatchObject({ status: 401 });
    expect(handler).not.toHaveBeenCalled();
  });

  it("reports 403 and network failures as ApiError", async () => {
    mockFetch(403, { error: "forbidden" });
    const forbidden = await api.servers().catch((e: unknown) => e);
    expect(forbidden).toBeInstanceOf(ApiError);
    expect(describeError(forbidden)).toMatch(/do not have access/);

    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("Failed to fetch")));
    const offline = await api.servers().catch((e: unknown) => e);
    expect(offline).toMatchObject({ status: 0 });
    expect(describeError(offline)).toMatch(/Cannot reach/);
  });

  it("falls back to the status text when the body is not JSON", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(new Response("<html>", { status: 502, statusText: "Bad Gateway" })),
    );
    await expect(api.servers()).rejects.toMatchObject({ status: 502, message: "Bad Gateway" });
  });
});
