"use client";

import Link from "next/link";
import { useCallback } from "react";

import { type OpenApiDocument, type OpenApiOperation, type OpenApiSchema, api, describeError } from "@/lib/api";
import { API_KEYS_PATH } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import admin from "./AdminPages.module.css";
import { ErrorState, LoadingBlock, PageHeader } from "./ui/PageState";

const METHODS = ["get", "post", "put", "patch", "delete"] as const;

/** The name of a referenced schema, `#/components/schemas/Name` → `Name`. */
function refName(ref: string): string {
  return ref.slice(ref.lastIndexOf("/") + 1);
}

/** A schema in a few words: `ServerView[]`, `string (uuid) or null`, `"a" | "b"`. */
export function typeWords(schema: OpenApiSchema | undefined): string {
  if (!schema) return "";
  if (schema.$ref) return refName(schema.$ref);
  if (schema.enum) return schema.enum.map((v) => JSON.stringify(v)).join(" | ");
  const alternatives = schema.oneOf ?? schema.allOf;
  if (alternatives) return alternatives.map(typeWords).join(" or ");
  const types = Array.isArray(schema.type) ? schema.type : schema.type ? [schema.type] : [];
  const words = types.map((t) => {
    if (t === "array") return `${typeWords(schema.items)}[]`;
    return schema.format && t !== "null" ? `${t} (${schema.format})` : t;
  });
  return words.join(" or ") || "any";
}

/** The operations of `doc` by tag, in the document's order. */
export function operationsByTag(
  doc: OpenApiDocument,
): [string, { method: string; path: string; op: OpenApiOperation }[]][] {
  const groups = new Map<string, { method: string; path: string; op: OpenApiOperation }[]>();
  for (const [path, item] of Object.entries(doc.paths)) {
    for (const method of METHODS) {
      const op = item[method];
      if (!op) continue;
      const tag = op.tags?.[0] ?? "other";
      groups.set(tag, [...(groups.get(tag) ?? []), { method, path, op }]);
    }
  }
  const order = (doc.tags ?? []).map((t) => t.name);
  const rank = (tag: string) => (order.includes(tag) ? order.indexOf(tag) : order.length);
  return [...groups.entries()].sort(([a], [b]) => rank(a) - rank(b));
}

/** `text` with its `backticked` words as code. */
export function WithCode({ text }: { text: string }) {
  return (
    <>
      {text.split("`").map((part, i) =>
        i % 2 === 1 ? (
          <code key={i} className={admin.mono}>
            {part}
          </code>
        ) : (
          part
        ),
      )}
    </>
  );
}

/** Answers every operation has, said once at the top instead. */
const COMMON_ERRORS = ["400", "401", "403", "404", "429"];

/**
 * `/admin/api-keys/docs/` (ADR-0101): the public API's OpenAPI document,
 * which the server generates from its code, as a page.
 */
export function ApiDocsPage() {
  const fetcher = useCallback((signal: AbortSignal) => api.openApi(signal), []);
  const { data, error, refresh } = usePolledData(fetcher);
  const header = (
    <PageHeader
      title="API documentation"
      meta={
        <>
          Version 1 at <code>/api/v1</code> · <a href="/api/v1/openapi.json">OpenAPI document</a> ·{" "}
          <Link href={API_KEYS_PATH}>API keys</Link>
        </>
      }
    />
  );
  if (!data) {
    return (
      <div className={admin.page}>
        {header}
        {error ? (
          <ErrorState what="the documentation" error={describeError(error)} onRetry={refresh} />
        ) : (
          <LoadingBlock />
        )}
      </div>
    );
  }
  const schemas = data.components?.schemas ?? {};
  return (
    <div className={admin.page}>
      {header}
      {data.info.description && (
        <p className={admin.footnote}>
          <WithCode text={data.info.description} />
        </p>
      )}
      <p className={admin.footnote}>
        Every operation may also answer {COMMON_ERRORS.join(", ")} with an <code className={admin.mono}>Error</code>:
        bad parameters, no valid key, not allowed, not found or outside the key&apos;s reach, and over the rate limit.
      </p>
      <pre className={admin.code}>{'curl -H "Authorization: Bearer mfk_…" https://<this server>/api/v1/servers'}</pre>
      {operationsByTag(data).map(([tag, ops]) => (
        <section key={tag} className={admin.section} aria-label={tag}>
          <div className={admin.sectionHead}>
            <h2 className={admin.sectionTitle}>{tag.charAt(0).toUpperCase() + tag.slice(1)}</h2>
            {description(data, tag) && <span className={admin.dim}>{description(data, tag)}</span>}
          </div>
          <div className={admin.sectionBody}>
            {ops.map(({ method, path, op }) => (
              <Operation key={`${method} ${path}`} method={method} path={path} op={op} />
            ))}
          </div>
        </section>
      ))}
      <section className={admin.section} aria-label="Types">
        <div className={admin.sectionHead}>
          <h2 className={admin.sectionTitle}>Types</h2>
        </div>
        <div className={admin.sectionBody}>
          {Object.entries(schemas).map(([name, schema]) => (
            <div key={name} id={`schema-${name}`}>
              <b className={admin.mono}>{name}</b>{" "}
              {schema.description && (
                <span className={admin.dim}>
                  <WithCode text={schema.description} />
                </span>
              )}
              {schema.properties ? (
                <ul className={admin.linkList}>
                  {Object.entries(schema.properties).map(([field, s]) => (
                    <li key={field}>
                      <span className={admin.mono}>{field}</span>{" "}
                      <span className={admin.dim}>
                        {typeWords(s)}
                        {schema.required?.includes(field) ? "" : ", optional"}
                        {s.description && (
                          <>
                            {" — "}
                            <WithCode text={s.description} />
                          </>
                        )}
                      </span>
                    </li>
                  ))}
                </ul>
              ) : (
                <span className={admin.dim}> {typeWords(schema)}</span>
              )}
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}

function description(doc: OpenApiDocument, tag: string): string | undefined {
  return doc.tags?.find((t) => t.name === tag)?.description;
}

function Operation({ method, path, op }: { method: string; path: string; op: OpenApiOperation }) {
  const body = op.requestBody?.content?.["application/json"]?.schema;
  return (
    <div>
      <div>
        <b className={admin.mono}>
          {method.toUpperCase()} {path}
        </b>
      </div>
      {(op.summary || op.description) && (
        <p className={admin.footnote}>
          <WithCode text={[op.summary, op.description].filter(Boolean).join(" ")} />
        </p>
      )}
      <ul className={admin.linkList}>
        {(op.parameters ?? []).map((p) => (
          <li key={`${p.in} ${p.name}`}>
            <span className={admin.mono}>{p.name}</span>{" "}
            <span className={admin.dim}>
              {p.in}, {typeWords(p.schema)}
              {p.required ? "" : ", optional"}
              {p.description && (
                <>
                  {" — "}
                  <WithCode text={p.description} />
                </>
              )}
            </span>
          </li>
        ))}
        {body && (
          <li>
            <span className={admin.mono}>body</span> <span className={admin.dim}>{typeWords(body)}</span>
          </li>
        )}
        {Object.entries(op.responses ?? {})
          .filter(([status]) => !COMMON_ERRORS.includes(status))
          .map(([status, response]) => (
            <li key={status}>
              <span className={admin.mono}>{status}</span>{" "}
              <span className={admin.dim}>
                {typeWords(response.content?.["application/json"]?.schema) || response.description}
              </span>
            </li>
          ))}
      </ul>
    </div>
  );
}
