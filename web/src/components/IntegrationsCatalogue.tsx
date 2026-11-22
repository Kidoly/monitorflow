"use client";

import Link from "next/link";

import { t } from "@/i18n";
import { type IntegrationList, type PsaConnection, api } from "@/lib/api";
import { administersEverything, isAdministrator, useMe } from "@/lib/me";
import { HALOPSA_PATH, WEBHOOKS_PATH } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./AdminPages.module.css";
import own from "./IntegrationsCatalogue.module.css";
import { integrationsStatus } from "./settings/status";
import { NoPermission, PageHeader } from "./ui/PageState";
import { Label } from "./ui/StatusLabel";
import type { Tone } from "./ui/tone";

export interface CardStatus {
  text: string;
  tone: Tone;
}

/** What the HaloPSA card says: off, failing, connected or saved; undefined when not set up. */
export function psaStatus(connection: PsaConnection | null | undefined): CardStatus | undefined {
  if (!connection) return undefined;
  if (!connection.enabled) return { text: t("psa.status.off"), tone: "neutral" };
  const failedLast =
    connection.secret_error !== null ||
    (connection.last_error_at !== null &&
      (connection.last_success_at === null || connection.last_error_at > connection.last_success_at));
  if (failedLast) return { text: t("psa.status.failing"), tone: "bad" };
  if (connection.agent_name && connection.last_success_at) {
    return { text: t("psa.status.connected", { agent: connection.agent_name }), tone: "ok" };
  }
  return { text: t("psa.status.notTested"), tone: "warn" };
}

/** What the custom webhook card says; undefined when there is none. */
export function webhookStatus(list: IntegrationList | undefined): CardStatus | undefined {
  return list ? integrationsStatus(list) : undefined;
}

interface Card {
  id: string;
  name: string;
  about: string;
  /** Where it is set up; none when the user may not. */
  href?: string;
  status?: CardStatus;
  note?: string;
}

/** `/settings/integrations/`: the catalogue, one card per integration, set-up ones first. */
export function IntegrationsCatalogue() {
  const me = useMe();
  const admin = isAdministrator(me);
  const global = administersEverything(me);
  const webhooks = usePolledData(api.integrations, { enabled: admin });
  const psa = usePolledData(api.psaConnection, { enabled: global });
  if (!admin) {
    return (
      <NoPermission title={t("integrations.needAdmin")}>{t("integrations.needAdminBody")}</NoPermission>
    );
  }
  const cards: Card[] = [
    {
      id: "halopsa",
      name: t("psa.catalogue.halopsa"),
      about: t("psa.catalogue.halopsaAbout"),
      href: global ? HALOPSA_PATH : undefined,
      status: psaStatus(psa.data?.connection),
      note: global ? undefined : t("psa.catalogue.globalOnly"),
    },
    {
      id: "webhook",
      name: t("psa.catalogue.webhook"),
      about: t("psa.catalogue.webhookAbout"),
      href: WEBHOOKS_PATH,
      status: webhookStatus(webhooks.data),
    },
  ];
  const configured = cards.filter((c) => c.status);
  const available = cards.filter((c) => !c.status);
  return (
    <div className={styles.page}>
      <PageHeader title={t("integrations.title")} />
      <p className={styles.footnote}>{t("psa.catalogue.intro")}</p>
      {configured.length > 0 && <Group title={t("psa.catalogue.configured")} cards={configured} />}
      {available.length > 0 && <Group title={t("psa.catalogue.available")} cards={available} />}
    </div>
  );
}

function Group({ title, cards }: { title: string; cards: Card[] }) {
  return (
    <section aria-label={title}>
      <h2 className={own.groupTitle}>{title}</h2>
      <ul className={own.grid}>
        {cards.map((card) => (
          <li key={card.id}>
            <CardBody card={card} />
          </li>
        ))}
      </ul>
    </section>
  );
}

function CardBody({ card }: { card: Card }) {
  const inner = (
    <>
      <span className={own.cardHead}>
        <span className={own.cardTitle}>{card.name}</span>
        {card.status && (
          <Label tone={card.status.tone} small>
            {card.status.text}
          </Label>
        )}
      </span>
      <span className={own.cardText}>{card.about}</span>
      <span className={own.cardAction}>
        {card.note ?? (card.status ? t("psa.catalogue.open") : t("psa.catalogue.setUp"))}
      </span>
    </>
  );
  return card.href ? (
    <Link className={own.card} href={card.href}>
      {inner}
    </Link>
  ) : (
    <div className={own.card}>{inner}</div>
  );
}
