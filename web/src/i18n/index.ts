import { Fragment, type ReactNode, createElement } from "react";

import { type Locale, activeLocale } from "./locale";
import type { Message, Plural } from "./catalog";
import { en, fr } from "./messages";

export { activeLocale } from "./locale";

const CATALOGS: Record<Locale, Record<string, Record<string, Message>>> = { en, fr };

type English = typeof en;

/** `area.name`: every message of the English catalog. */
export type MessageKey = {
  [A in keyof English & string]: `${A}.${keyof English[A] & string}`;
}[keyof English & string];

type MessageAt<K> = K extends `${infer A}.${infer N}`
  ? A extends keyof English
    ? N extends keyof English[A]
      ? English[A][N]
      : never
    : never
  : never;

type Names<S> = S extends `${string}{${infer P}}${infer R}` ? P | Names<R> : never;

type ParamsOf<M> = M extends string
  ? Names<M>
  : M extends { one: infer O; other: infer X }
    ? "count" | Names<O> | Names<X>
    : never;

/** The `{placeholders}` of `K`'s English message; `count` for a plural. */
export type Params<K extends MessageKey> = ParamsOf<MessageAt<K>>;

/** The keys of messages without placeholders, for a label kept as data. */
export type PlainKey = { [K in MessageKey]: [Params<K>] extends [never] ? K : never }[MessageKey];

type Args<K extends MessageKey, V> = [Params<K>] extends [never] ? [] : [values: Record<Params<K>, V>];

const numbers = new Map<Locale, Intl.NumberFormat>();
const plurals = new Map<Locale, Intl.PluralRules>();

function cached<T>(map: Map<Locale, T>, locale: Locale, make: () => T): T {
  let value = map.get(locale);
  if (!value) {
    value = make();
    map.set(locale, value);
  }
  return value;
}

function lookup(locale: Locale, key: string): Message | undefined {
  const dot = key.indexOf(".");
  return CATALOGS[locale][key.slice(0, dot)]?.[key.slice(dot + 1)];
}

/** The text of `key` in the active language, English when it has none, the plural form picked by `count`. */
function template(key: string, count: unknown): string {
  const locale = activeLocale();
  const message = lookup(locale, key) ?? lookup("en", key);
  if (message === undefined) return key;
  if (typeof message === "string") return message;
  const form = cached(plurals, locale, () => new Intl.PluralRules(locale)).select(Number(count)) as keyof Plural;
  return message[form] ?? message.other;
}

function show(value: unknown): string {
  if (typeof value !== "number") return String(value);
  const locale = activeLocale();
  return cached(numbers, locale, () => new Intl.NumberFormat(locale, { maximumFractionDigits: 2 })).format(value);
}

const PLACEHOLDER = /\{(\w+)\}/g;

/**
 * The message `key` in the active language, its `{placeholders}` filled
 * from `values`. Numbers are quantities, formatted for the language
 * ("1 234" in French): pass a port, a year or an ID as a string.
 */
export function t<K extends MessageKey>(key: K, ...[values]: Args<K, string | number>): string {
  const all = (values ?? {}) as Record<string, unknown>;
  return template(key, all.count).replace(PLACEHOLDER, (whole, name: string) =>
    name in all ? show(all[name]) : whole,
  );
}

/** As `t`, for placeholders that are elements, like a link inside a sentence. */
export function rich<K extends MessageKey>(key: K, ...[values]: Args<K, ReactNode>): ReactNode {
  const all = (values ?? {}) as Record<string, ReactNode>;
  const parts = template(key, all.count).split(PLACEHOLDER);
  return createElement(
    Fragment,
    null,
    ...parts.map((part, index) => {
      if (index % 2 === 0) return part;
      if (!(part in all)) return `{${part}}`;
      const value = all[part];
      return typeof value === "number" ? show(value) : value;
    }),
  );
}
