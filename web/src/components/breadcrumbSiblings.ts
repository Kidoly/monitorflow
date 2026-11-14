import { type EffectiveSensor, type SensorValueList, type ServerDetail, api } from "@/lib/api";
import { sensorHref, serverHref } from "@/lib/routes";
import { type Scope, scopedHref } from "@/lib/scope";
import { primaryMetric, seriesTitle } from "@/lib/sensors";
import type { Sibling, Siblings } from "@/components/ui/Breadcrumb";

/** The other servers of `server`'s site, loaded when the list opens. */
export function serverSiblings(server: ServerDetail, scope: Scope): Siblings {
  return {
    label: `Other servers of ${server.site_name}`,
    load: async (signal) => {
      const { servers } = await api.siteServers(server.site_id, signal);
      return servers.map((s) => ({ label: s.hostname, href: scopedHref(serverHref(s.id), scope), current: s.id === server.id }));
    },
  };
}

/**
 * The other sensors of `server`: one entry per sensor and instance seen,
 * like the server's sensor table, from what the page already loaded.
 */
export function sensorSiblings(
  server: ServerDetail,
  sensors: EffectiveSensor[],
  values: SensorValueList | null,
  current: { metric: string; instance: string },
  scope: Scope,
): Siblings {
  const list: Sibling[] = sensors.flatMap((sensor) => {
    const { metric, instance: fixed } = primaryMetric(sensor);
    const entry = values?.sensors.find((v) => v.name === sensor.name && v.instance === sensor.instance);
    const instances = entry && entry.series.length > 0 ? entry.series.map((s) => s.instance) : [fixed ?? ""];
    // The same links as the server's sensor table.
    return instances.map((instance) => ({
      label: seriesTitle(sensor, metric, instance),
      href: scopedHref(sensorHref(server.id, metric, instance), scope),
      current: metric === current.metric && instance === current.instance,
    }));
  });
  const byLabel = new Intl.Collator("en", { numeric: true });
  list.sort((a, b) => byLabel.compare(a.label, b.label));
  return { label: `Other sensors of ${server.hostname}`, load: async () => list };
}
