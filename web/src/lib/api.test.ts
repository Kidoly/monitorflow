import { afterEach, describe, expect, it, vi } from "vitest";

import { stubFetch } from "../test/fetch";
import { ApiError, CSRF_HEADER, type Server, api, describeError, setUnauthorizedHandler } from "./api";

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

  it("follows next_cursor until the last page of servers", async () => {
    const server = (peer_id: string) => ({ peer_id }) as Server;
    const fetchMock = stubFetch({
      "GET /api/servers": { status: 200, body: { servers: [server("a")], next_cursor: "c/1" } },
      "GET /api/servers?cursor=c%2F1": { status: 200, body: { servers: [server("b")], next_cursor: null } },
    });
    const { servers, next_cursor } = await api.servers();
    expect(servers.map((s) => s.peer_id)).toEqual(["a", "b"]);
    expect(next_cursor).toBeNull();
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("builds metric query parameters", async () => {
    const fetchMock = mockFetch(200, { query: "q", series: [] });
    await api.queryRange({
      serverId: "s 1",
      metric: "monitorflow_disk_used_ratio",
      instance: "/var/lib",
      start: new Date("2026-10-19T11:00:00Z"),
      end: new Date("2026-10-19T12:00:00Z"),
      step: 30,
      agg: "max",
    });
    expect(fetchMock.mock.calls[0]?.[0]).toBe(
      "/api/metrics/query_range?server_id=s+1&metric=monitorflow_disk_used_ratio&instance=%2Fvar%2Flib" +
        "&start=2026-10-19T11%3A00%3A00.000Z&end=2026-10-19T12%3A00%3A00.000Z&step=30&agg=max",
    );
    await api.serverMetrics("a/b");
    expect(fetchMock.mock.calls[1]?.[0]).toBe("/api/servers/a%2Fb/metrics");
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
