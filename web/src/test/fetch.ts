import { vi } from "vitest";

type Reply = { status: number; body?: unknown } | Error;

/** Stubs `fetch` with replies keyed by `METHOD path`, used in order when a key has several. */
export function stubFetch(routes: Record<string, Reply | Reply[]>) {
  const queues = new Map(Object.entries(routes).map(([k, v]) => [k, Array.isArray(v) ? [...v] : [v]]));
  const fetchMock = vi.fn<typeof fetch>(async (input, init) => {
    const key = `${init?.method ?? "GET"} ${String(input)}`;
    const queue = queues.get(key);
    const reply = queue && (queue.length > 1 ? queue.shift() : queue[0]);
    if (!reply) {
      throw new Error(`unexpected request ${key}`);
    }
    if (reply instanceof Error) {
      throw reply;
    }
    return new Response(reply.body === undefined ? null : JSON.stringify(reply.body), {
      status: reply.status,
      headers: { "Content-Type": "application/json" },
    });
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

/** Headers of the `index`-th call to a stubbed `fetch`. */
export function requestHeaders(fetchMock: ReturnType<typeof stubFetch>, index = 0) {
  return fetchMock.mock.calls[index]?.[1]?.headers as Record<string, string>;
}
