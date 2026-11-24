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
  // Stacked waves: a campaign rolls an update out in waves.
  campaigns: <path d="M8 2l6 3-6 3-6-3zM2 8l6 3 6-3M2 11l6 3 6-3" />,
  // Two turning arrows: updates that apply themselves.
  autoUpdate: <path d="M13.5 6A5.7 5.7 0 0 0 3 4.5M2.5 2v3h3M2.5 10a5.7 5.7 0 0 0 10.5 1.5M13.5 14v-3h-3" />,
  approvals: <path d="M3 8l3 3 7-7" />,
  audit: <path d="M4 3h8v10H4zM6 6h4M6 9h4" />,
  // A prompt and a line: a script of the library.
  scripts: <path d="M2 3h12v10H2zM4.5 6l2 2-2 2M8 10h3.5" />,
  // A lightning bolt: something happens, a script runs.
  automation: <path d="M9 2L4 9h4l-1 5 5-7H8z" />,
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
  signIn: <path d="M9 2h4v12H9M2 8h8M7 5l3 3-3 3" />,
  ai: <path d="M8 2l1.5 4.5L14 8l-4.5 1.5L8 14l-1.5-4.5L2 8l4.5-1.5z" />,
  bell: <path d="M4 11V7a4 4 0 0 1 8 0v4l1 1H3zM7 14h2" />,
  reports: <path d="M3 2h7l3 3v9H3zM6 11V8M8 11V6M10 11V9" />,
  inventory: <path d="M2 5l6-3 6 3v6l-6 3-6-3zM2 5l6 3 6-3M8 8v6" />,
  dashboards: <path d="M2 2h5v6H2zM9 2h5v3H9zM9 7h5v7H9zM2 10h5v4H2z" />,
  maps: <path d="M8 2.5v4M8 6.5L3.5 11M8 6.5l4.5 4.5M3.5 13h9M8 1a1.5 1.5 0 1 1 0 3 1.5 1.5 0 0 1 0-3zM3.5 11.5a1.5 1.5 0 1 1 0 3 1.5 1.5 0 0 1 0-3zM12.5 11.5a1.5 1.5 0 1 1 0 3 1.5 1.5 0 0 1 0-3z" />,
  backups: <path d="M3 4h10v3H3zM4 7v6h8V7M7 10h2" />,
  plugin: <path d="M6 2v3M10 2v3M4 5h8v3a4 4 0 0 1-8 0zM8 12v2" />,
  authority: <path d="M8 2l5 2v4c0 3-2.2 5-5 6-2.8-1-5-3-5-6V4zM6 8l1.5 1.5L10.5 6.5" />,
  integrations: <path d="M6 2v3M10 2v3M4 5h8v2a4 4 0 0 1-8 0zM8 11v3" />,
  license: <path d="M3 3h10v10H3zM5.5 6h5M5.5 8.5h5M8 11h2.5" />,
  settings: (
    <>
      <path d="M2 4.5h7M12 4.5h2M2 11.5h2M7 11.5h7" />
      <circle cx="10.5" cy="4.5" r="1.5" />
      <circle cx="5.5" cy="11.5" r="1.5" />
    </>
  ),
  chevron: <path d="M4 6l4 4 4-4" />,
  back: <path d="M13 8H3M7 4L3 8l4 4" />,
  scope: (
    <>
      <rect x="3" y="2" width="10" height="12" rx="1" />
      <path d="M6 5h1M9 5h1M6 8h1M9 8h1M7 14v-3h2v3" />
    </>
  ),
  menu: <path d="M2 4h12M2 8h12M2 12h12" />,
  collapse: <path d="M3 2v12M12 4L8 8l4 4" />,
  expand: <path d="M3 2v12M8 4l4 4-4 4" />,
  personalize: <path d="M2 4h7M12 4h2M2 12h2M7 12h7M9 2v4M11 2v4M4 10v4M6 10v4" />,
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
