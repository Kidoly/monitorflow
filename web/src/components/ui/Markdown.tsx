import type { ReactNode } from "react";

import { cx } from "./cx";
import styles from "./Markdown.module.css";

/** Schemes a link may use besides a page of this site. */
const LINK_SCHEMES = new Set(["http:", "https:", "mailto:"]);

/**
 * A link's target when it is one we follow: http(s), mail, or a page of this site.
 * The target is rebuilt from the parsed URL, never passed on as written: a page of
 * this site keeps only its path, query and fragment, so it cannot name another host.
 */
export function safeHref(href: string): string | null {
  const target = href.trim();
  if (target.startsWith("/") && !target.startsWith("//")) {
    const url = new URL(target, "https://site.invalid");
    return `${url.pathname}${url.search}${url.hash}`;
  }
  let url: URL;
  try {
    url = new URL(target);
  } catch {
    return null;
  }
  return LINK_SCHEMES.has(url.protocol) ? url.href : null;
}

const INLINE = /(\*\*([^*]+)\*\*|__([^_]+)__|\*([^*]+)\*|_([^_]+)_|`([^`]+)`|\[([^\]]+)\]\(([^)\s]+)\))/;

/**
 * The AI's marks (ADR-0181): `**bold**` and `` `code` `` only. Its text is untrusted, so it
 * gets no links, and single `*` or `_` stay as written: they are in paths and globs
 * (`/var/log/*.gz`, `__pycache__`) far more often than meant as italics. Its groups are
 * those of `INLINE`; `(?!)` never matches, so the alternatives it leaves out never apply.
 */
const AI_INLINE = /(\*\*([^*]+)\*\*|(?!)()|(?!)()|(?!)()|`([^`]+)`|(?!)()())/;

/** Which part of Markdown a text gets: all of it for panels people write, a smaller one for the AI's words. */
export type MarkdownVariant = "panel" | "ai";

/** Bold, italics, code and links of one line, as React elements: nothing is read as HTML. */
export function inline(text: string, key = "i", variant: MarkdownVariant = "panel"): ReactNode[] {
  const pattern = variant === "ai" ? AI_INLINE : INLINE;
  const out: ReactNode[] = [];
  let rest = text;
  let n = 0;
  for (;;) {
    const match = pattern.exec(rest);
    if (!match) break;
    if (match.index > 0) out.push(rest.slice(0, match.index));
    const k = `${key}-${n++}`;
    const [, , bold1, bold2, em1, em2, code, label, href] = match;
    if (bold1 ?? bold2) out.push(<strong key={k}>{inline(bold1 ?? bold2!, k, variant)}</strong>);
    else if (em1 ?? em2) out.push(<em key={k}>{inline(em1 ?? em2!, k, variant)}</em>);
    else if (code) out.push(<code key={k}>{code}</code>);
    else {
      const target = safeHref(href!);
      out.push(
        target ? (
          <a key={k} href={target} rel="noopener noreferrer" target={target.startsWith("/") ? undefined : "_blank"}>
            {label}
          </a>
        ) : (
          label
        ),
      );
    }
    rest = rest.slice(match.index + match[0].length);
  }
  if (rest) out.push(rest);
  return out;
}

/**
 * A small part of Markdown for text panels (ADR-0178): headings, lists,
 * paragraphs, bold, italics, code and links. Built from React elements,
 * never from HTML, so a dashboard's text cannot run anything.
 *
 * The `ai` variant, for the AI's diagnosis (ADR-0181), keeps paragraphs,
 * lists, bold and inline code: a heading line stays a plain line, a link
 * stays its text. `lead`, like a label, starts the first paragraph.
 */
export function Markdown({
  text,
  variant = "panel",
  lead,
  className,
}: {
  text: string;
  variant?: MarkdownVariant;
  lead?: ReactNode;
  className?: string;
}) {
  const ai = variant === "ai";
  const inline_ = (line: string, key: string) => inline(line, key, variant);
  // A paragraph keeps its lines apart, so the lead can start the first one.
  const blocks: ({ key: string; lines: ReactNode[] } | { key: string; node: ReactNode })[] = [];
  let list: { ordered: boolean; items: string[] } | null = null;
  let paragraph: string[] = [];
  const flush = () => {
    if (paragraph.length) {
      const k = `p${blocks.length}`;
      const lines = paragraph.flatMap((line, i) => (i ? [<br key={`${k}-br${i}`} />, ...inline_(line, `${k}-${i}`)] : inline_(line, `${k}-${i}`)));
      blocks.push({ key: k, lines });
      paragraph = [];
    }
    if (list) {
      const k = `l${blocks.length}`;
      const items = list.items.map((item, i) => <li key={i}>{inline_(item, `${k}-${i}`)}</li>);
      blocks.push({ key: k, node: list.ordered ? <ol key={k}>{items}</ol> : <ul key={k}>{items}</ul> });
      list = null;
    }
  };
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trimEnd();
    const heading = ai ? null : /^(#{1,3})\s+(.*)$/.exec(line);
    const bullet = /^\s*[-*]\s+(.*)$/.exec(line);
    const numbered = /^\s*\d+[.)]\s+(.*)$/.exec(line);
    if (!line.trim()) {
      flush();
    } else if (heading) {
      flush();
      const k = `h${blocks.length}`;
      const content = inline_(heading[2]!, k);
      blocks.push({ key: k, node: heading[1]!.length === 1 ? <h3 key={k}>{content}</h3> : <h4 key={k}>{content}</h4> });
    } else if (bullet || numbered) {
      if (paragraph.length) flush();
      const ordered = Boolean(numbered);
      if (list && list.ordered !== ordered) flush();
      list ??= { ordered, items: [] };
      list.items.push((bullet ?? numbered)![1]!);
    } else {
      if (list) flush();
      paragraph.push(line);
    }
  }
  flush();
  const leadNode = lead === undefined ? null : <span key="lead">{lead}</span>;
  const rendered = blocks.map((block, i) =>
    "lines" in block ? (
      <p key={block.key}>{i === 0 && leadNode ? [leadNode, " ", ...block.lines] : block.lines}</p>
    ) : (
      block.node
    ),
  );
  if (leadNode && !(blocks[0] && "lines" in blocks[0])) rendered.unshift(<p key="lead">{leadNode}</p>);
  return <div className={cx(styles.text, className)}>{rendered}</div>;
}
