import styles from "./Reason.module.css";

/**
 * Why something is not OK, in the server's sentence. `short` keeps it to
 * one line for a table cell; the full sentence stays in the tooltip and
 * the accessible text.
 */
export function Reason({ text, short }: { text: string | null | undefined; short?: boolean }) {
  if (!text) return null;
  return (
    <span className={short ? styles.short : styles.reason} title={short ? text : undefined}>
      {text}
    </span>
  );
}
