import Link from "next/link";
import type { ComponentProps } from "react";

import styles from "./Button.module.css";
import { cx } from "./cx";

/** `default` is bordered white, `k` is the black primary, `danger` the red one. */
export type ButtonVariant = "default" | "k" | "danger";
export type ButtonSize = "default" | "small" | "large";

interface Look {
  variant?: ButtonVariant;
  size?: ButtonSize;
  /** Draws an active filter: black border. */
  active?: boolean;
}

/**
 * The class names of a button look, for elements that are not a `Button`
 * or `ButtonLink` (an `<input>` or `<select>` styled as a 36 px control).
 */
export function buttonClass({ variant = "default", size = "default", active }: Look = {}, extra?: string): string {
  return cx(
    styles.btn,
    variant !== "default" && styles[variant],
    size !== "default" && styles[size],
    active && styles.active,
    extra,
  );
}

/** The class that makes an input or select look like a 36 px field. */
export const fieldClass = cx(styles.btn, styles.field);

type ButtonProps = Look & ComponentProps<"button">;

/** A `<button>`, `type="button"` unless told otherwise. */
export function Button({ variant, size, active, className, type = "button", ...props }: ButtonProps) {
  return <button type={type} className={buttonClass({ variant, size, active }, className)} {...props} />;
}

type LinkProps = Look & ComponentProps<typeof Link>;

/** A Next `<Link>` drawn as a button (`.btn-k` on `<a>` in the handoff). */
export function ButtonLink({ variant, size, active, className, ...props }: LinkProps) {
  return <Link className={buttonClass({ variant, size, active }, className)} {...props} />;
}
