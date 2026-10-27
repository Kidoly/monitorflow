import type { ReactNode } from "react";

// The only icons of the product: 16 px, 1.6 stroke, sidebar only. The
// first six are the handoff's; the rest follow the same grid for the
// collapsed (< 1100 px) sidebar, where every item needs one.
const PATHS = {
  fleet: (
    <>
      <rect x="2" y="3" width="12" height="4" rx="1" />
      <rect x="2" y="9" width="12" height="4" rx="1" />
    </>
  ),
  incidents: (
    <>
      <circle cx="8" cy="8" r="6" />
      <path d="M8 5v3.5M8 11v.5" />
    </>
  ),
  mirrors: (
    <>
      <circle cx="8" cy="8" r="5.5" />
      <path d="M8 5v3l2 1" />
    </>
  ),
  campaigns: <path d="M3 8h10M9 4l4 4-4 4" />,
  approvals: <path d="M3 8l3 3 7-7" />,
  audit: <path d="M4 3h8v10H4zM6 6h4M6 9h4" />,
  relays: (
    <>
      <circle cx="4" cy="8" r="2" />
      <circle cx="12" cy="4" r="2" />
      <circle cx="12" cy="12" r="2" />
      <path d="M6 7l4-2M6 9l4 2" />
    </>
  ),
  sites: <path d="M2 14h12M3 14V5l5-3 5 3v9M6 14v-3h4v3" />,
  tokens: (
    <>
      <circle cx="5" cy="8" r="3" />
      <path d="M8 8h6M12 8v2.5M14 8v2" />
    </>
  ),
  users: (
    <>
      <circle cx="6" cy="5.5" r="2.5" />
      <path d="M1.5 13.5c.5-2.5 2.3-4 4.5-4s4 1.5 4.5 4M11 3a2.5 2.5 0 0 1 0 5M12.5 9.7c1.2.6 1.9 1.9 2 3.8" />
    </>
  ),
  update: <path d="M8 13V3M4 7l4-4 4 4M3 13h10" />,
  chevron: <path d="M4 6l4 4 4-4" />,
  scope: (
    <>
      <rect x="3" y="2" width="10" height="12" rx="1" />
      <path d="M6 5h1M9 5h1M6 8h1M9 8h1M7 14v-3h2v3" />
    </>
  ),
  theme: (
    <>
      <circle cx="8" cy="8" r="6" />
      <path d="M8 2a6 6 0 0 0 0 12z" />
    </>
  ),
  signOut: <path d="M6 3H3v10h3M10 5l3 3-3 3M13 8H6" />,
  menu: <path d="M2 4h12M2 8h12M2 12h12" />,
} satisfies Record<string, ReactNode>;

export type IconName = keyof typeof PATHS;

export function Icon({ name, className }: { name: IconName; className?: string }) {
  return (
    <svg
      className={className}
      width="16"
      height="16"
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      {PATHS[name]}
    </svg>
  );
}
