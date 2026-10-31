import type { ServerDetail } from "@/lib/api";
import { readsCustomers, useMe } from "@/lib/me";
import { CAMPAIGNS_PATH } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { ButtonLink } from "@/components/ui/Button";
import { EmptyState } from "@/components/ui/PageState";

/**
 * The Updates tab. The server page had no updates section before the
 * redesign; the patch workflow of the handoff (03c) replaces this file.
 */
export function ServerUpdatesTab({ server }: { server: ServerDetail }) {
  const me = useMe();
  const scope = useScope();
  if (server.package_updates === false) {
    return (
      <EmptyState title="Package updates aren't available on this system">
        Monitoring works normally. MonitorFlow applies updates on Debian, RHEL and compatible systems; update {server.hostname} with
        its own tools.
      </EmptyState>
    );
  }
  return (
    <EmptyState
      actions={
        readsCustomers(me) && <ButtonLink href={scopedHref(CAMPAIGNS_PATH, scope)}>Open campaigns</ButtonLink>
      }
    >
      Updates for {server.hostname} are applied through update campaigns for now.
    </EmptyState>
  );
}
