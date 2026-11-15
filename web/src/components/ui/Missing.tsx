import { t } from "@/i18n";

/** What a table cell or a fact says when it has no value, in a word that tells why. */
export type MissingWord = "none" | "notSet" | "never" | "unknown" | "notYet" | "noData";

/** The word for a missing value, as text: "Never", "Jamais". */
export const missingText = (word: MissingWord): string => t(`ui.missing.${word}`);

/** The word for a missing value, usually drawn dimmed by the page's own class. */
export function Missing({ word, className }: { word: MissingWord; className?: string }) {
  return <span className={className}>{missingText(word)}</span>;
}
