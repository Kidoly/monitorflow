import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { OpenApiDocument } from "@/lib/api";
import { stubFetch } from "@/test/fetch";

import { ApiDocsPage, operationsByTag, typeWords } from "./ApiDocs";

const DOC: OpenApiDocument = {
  info: {
    title: "MonitorFlow public API",
    version: "1",
    description: "Use a key.",
  },
  paths: {
    "/api/v1/servers": {
      get: {
        tags: ["servers"],
        parameters: [
          {
            name: "limit",
            in: "query",
            required: false,
            schema: { type: ["integer", "null"] },
          },
        ],
        responses: {
          "200": {
            description: "",
            content: {
              "application/json": {
                schema: { $ref: "#/components/schemas/ServerList" },
              },
            },
          },
        },
      },
    },
    "/api/v1/incidents/acknowledge": {
      post: {
        tags: ["alerts"],
        description: "Says someone is on it.",
        requestBody: {
          content: {
            "application/json": {
              schema: { $ref: "#/components/schemas/AcknowledgeRequest" },
            },
          },
        },
        responses: { "200": { description: "" }, "404": { description: "Not found" } },
      },
    },
  },
  tags: [
    { name: "alerts", description: "Incidents and silences" },
    { name: "servers", description: "Servers" },
  ],
  components: {
    schemas: {
      ServerList: {
        type: "object",
        required: ["servers"],
        properties: {
          servers: {
            type: "array",
            items: { $ref: "#/components/schemas/ServerView" },
          },
          next_cursor: {
            type: ["string", "null"],
            description: "Pass as `cursor`.",
          },
        },
      },
      Error: {
        type: "object",
        properties: {
          code: { type: "string", enum: ["not_found", "forbidden"] },
        },
      },
    },
  },
};

afterEach(() => vi.unstubAllGlobals());

describe("typeWords", () => {
  it("says a schema in a few words", () => {
    expect(typeWords({ $ref: "#/components/schemas/ServerView" })).toBe("ServerView");
    expect(
      typeWords({
        type: "array",
        items: { $ref: "#/components/schemas/ServerView" },
      }),
    ).toBe("ServerView[]");
    expect(typeWords({ type: ["string", "null"], format: "uuid" })).toBe("string (uuid) or null");
    expect(typeWords({ type: "string", enum: ["a", "b"] })).toBe('"a" | "b"');
    expect(typeWords({})).toBe("any");
  });

  it("groups operations by tag", () => {
    expect(operationsByTag(DOC).map(([tag, ops]) => [tag, ops.map((o) => `${o.method} ${o.path}`)])).toEqual([
      ["alerts", ["post /api/v1/incidents/acknowledge"]],
      ["servers", ["get /api/v1/servers"]],
    ]);
  });
});

describe("ApiDocsPage", () => {
  it("shows the operations and types of the served document", async () => {
    stubFetch({ "GET /api/v1/openapi.json": { status: 200, body: DOC } });
    render(<ApiDocsPage />);
    expect(await screen.findByText("GET /api/v1/servers")).toBeInTheDocument();
    expect(screen.getByText("POST /api/v1/incidents/acknowledge")).toBeInTheDocument();
    expect(screen.getByText("Says someone is on it.")).toBeInTheDocument();
    expect(screen.getByText("AcknowledgeRequest")).toBeInTheDocument();
    expect(screen.getByText(/ServerView\[\]/)).toBeInTheDocument();
    expect(screen.getByText(/"not_found" \| "forbidden"/)).toBeInTheDocument();
    expect(screen.getByText("Incidents and silences")).toBeInTheDocument();
    // Common errors are said once, not under every operation.
    expect(screen.queryByText("Not found")).not.toBeInTheDocument();
    expect(screen.getByText("cursor", { selector: "code" })).toBeInTheDocument();
  });
});
