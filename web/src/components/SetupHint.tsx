import { CopyButton } from "@/components/ui/SecretField";
import { t } from "@/i18n";

import styles from "./LoginForm.module.css";

/** Prints a fresh setup link on the server host (ADR-0071). */
export const SETUP_LINK_COMMAND = "docker compose -f deploy/docker-compose.yml exec server monitorflow-server setup-link";

/** How to get a setup link, shown while no account exists. */
export function SetupHint() {
  return (
    <>
      <p className={styles.text}>
        {t("auth.setupHint.text")}
      </p>
      <div className={styles.command}>
        <code>{SETUP_LINK_COMMAND}</code>
        <CopyButton text={SETUP_LINK_COMMAND} name={t("auth.setupHint.copy")} size="small" />
      </div>
    </>
  );
}
