/**
 * A message whose words follow a count, picked by `Intl.PluralRules`:
 * French says "one" for 0 and 1, and may say "many" for millions.
 * `{count}` is the number, formatted for the language.
 */
export interface Plural {
  one: string;
  other: string;
  many?: string;
}

export type Message = string | Plural;

/** The messages of one area, English, read-only so their placeholders are typed. */
export type Catalog = Readonly<Record<string, string | Readonly<Plural>>>;

/** Another language's messages of an area: every English key, nothing else. */
export type Translation<T extends Catalog> = { [K in keyof T]: T[K] extends string ? string : Plural };
