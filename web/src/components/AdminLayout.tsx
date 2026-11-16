import Link from "next/link";
import type { ReactNode } from "react";

import { NoPermission } from "@/components/ui/PageState";
import { rich, t } from "@/i18n";
import { type Me } from "@/lib/api";
import { isAdministrator, useMe } from "@/lib/me";

/**
 * `children` for administrators; the sidebar's Settings list is
 * the navigation between these pages. Other users get a note instead (the
 * sidebar hides the section from them, so only a direct link lands here);
 * the API refuses them anyway (ADR-0029).
 */
export function AdminLayout({ children }: { children: (me: Me) => ReactNode }) {
  const me = useMe();
  if (!me || !isAdministrator(me)) {
    return (
      <NoPermission title={t("auth.admin.title")}>
        {me && <>{rich("auth.admin.signedInAs", { username: <strong>{me.username}</strong> })} </>}
        {rich("auth.admin.ask", { fleet: <Link href="/">{t("auth.admin.fleet")}</Link> })}
      </NoPermission>
    );
  }
  return <>{children(me)}</>;
}
