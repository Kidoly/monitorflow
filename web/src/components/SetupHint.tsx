import { CopyButton } from "@/components/ui/SecretField";

import styles from "./LoginForm.module.css";

/** Prints a fresh setup link on the server host (ADR-0057). */
export const SETUP_LINK_COMMAND = "docker compose -f deploy/docker-compose.yml exec server monitorflow-server setup-link";

/** How to get a setup link, shown while no account exists. */
export function SetupHint() {
  return (
    <>
      <p className={styles.text}>
        Open the setup link printed by the installer or in the server log. To print a new one, run this on the server:
      </p>
      <div className={styles.command}>
        <code>{SETUP_LINK_COMMAND}</code>
        <CopyButton text={SETUP_LINK_COMMAND} name="Copy the command" size="small" />
      </div>
    </>
  );
}
