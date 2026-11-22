import type { ReactNode } from "react";

import styles from "./Markdown.module.css";

/** A link's target when it is one we follow: http(s), mail, or a page of this site. */
export function safeHref(href: string): string | null {
  const target = href.trim();
  if (/^(https?:\/\/|mailto:)/i.test(target)) return target;
  if (target.startsWith("/") && !target.startsWith("//")) return target;
  return null;
}

const INLINE = /(\*\*([^*]+)\*\*|__([^_]+)__|\*([^*]+)\*|_([^_]+)_|`([^`]+)`|\[([^\]]+)\]\(([^)\s]+)\))/;

/** Bold, italics, code and links of one line, as React elements: nothing is read as HTML. */
export function inline(text: string, key = "i"): ReactNode[] {
  const out: ReactNode[] = [];
  let rest = text;
  let n = 0;
  for (;;) {
    const match = INLINE.exec(rest);
    if (!match) break;
    if (match.index > 0) out.push(rest.slice(0, match.index));
    const k = `${key}-${n++}`;
    const [, , bold1, bold2, em1, em2, code, label, href] = match;
    if (bold1 ?? bold2) out.push(<strong key={k}>{inline(bold1 ?? bold2!, k)}</strong>);
    else if (em1 ?? em2) out.push(<em key={k}>{inline(em1 ?? em2!, k)}</em>);
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
 * A small part of Markdown for text panels (ADR-0166): headings, lists,
 * paragraphs, bold, italics, code and links. Built from React elements,
 * never from HTML, so a dashboard's text cannot run anything.
 */
export function Markdown({ text }: { text: string }) {
  const blocks: ReactNode[] = [];
  let list: { ordered: boolean; items: string[] } | null = null;
  let paragraph: string[] = [];
  const flush = () => {
    if (paragraph.length) {
      const k = `p${blocks.length}`;
      blocks.push(<p key={k}>{paragraph.flatMap((line, i) => (i ? [<br key={`${k}-br${i}`} />, ...inline(line, `${k}-${i}`)] : inline(line, `${k}-${i}`)))}</p>);
      paragraph = [];
    }
    if (list) {
      const k = `l${blocks.length}`;
      const items = list.items.map((item, i) => <li key={i}>{inline(item, `${k}-${i}`)}</li>);
      blocks.push(list.ordered ? <ol key={k}>{items}</ol> : <ul key={k}>{items}</ul>);
      list = null;
    }
  };
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trimEnd();
    const heading = /^(#{1,3})\s+(.*)$/.exec(line);
    const bullet = /^\s*[-*]\s+(.*)$/.exec(line);
    const numbered = /^\s*\d+[.)]\s+(.*)$/.exec(line);
    if (!line.trim()) {
      flush();
    } else if (heading) {
      flush();
      const k = `h${blocks.length}`;
      const content = inline(heading[2]!, k);
      blocks.push(heading[1]!.length === 1 ? <h3 key={k}>{content}</h3> : <h4 key={k}>{content}</h4>);
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
  return <div className={styles.text}>{blocks}</div>;
}
