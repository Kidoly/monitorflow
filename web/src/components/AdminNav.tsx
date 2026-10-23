import Link from "next/link";
import type { ReactNode } from "react";

import { NoPermission } from "@/components/ui/PageState";
import { type Me } from "@/lib/api";
import { isAdministrator, useMe } from "@/lib/me";

/**
 * `children` for administrators; the sidebar's Administration section is
 * the navigation between these pages. Other users get a note instead (the
 * sidebar hides the section from them, so only a direct link lands here);
 * the API refuses them anyway (ADR-0029).
 */
export function AdminLayout({ children }: { children: (me: Me) => ReactNode }) {
  const me = useMe();
  if (!me || !isAdministrator(me)) {
    return (
      <NoPermission title="Administration needs an administrator role.">
        {me && (
          <>
            You are signed in as <strong>{me.username}</strong>.{" "}
          </>
        )}
        Ask an administrator to grant you access, or go back to <Link href="/">Fleet</Link>.
      </NoPermission>
    );
  }
  return <>{children(me)}</>;
}
