"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";

import { Banner } from "@/components/ui/PageState";
import { api } from "@/lib/api";
import { authorityNotice } from "@/lib/certificateAuthority";
import { CERTIFICATE_AUTHORITY_PATH } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./Shell.module.css";

/** An emergency or a restore is worth knowing within minutes; expiry takes months. */
const REFRESH_MS = 10 * 60 * 1000;

/**
 * Tells global administrators, on every page, about the certificate
 * authority when it needs them (ADR-0088): an emergency change under way,
 * a restore that brought back a retired one, machines that need a new
 * install, or an authority close to its end. The page has the rest.
 */
export function CertificateAuthorityBanner() {
  const pathname = usePathname();
  const view = usePolledData(api.certificateAuthority, { intervalMs: REFRESH_MS }).data;
  const notice = view ? authorityNotice(view) : null;
  if (!notice || pathname.startsWith(CERTIFICATE_AUTHORITY_PATH.slice(0, -1))) return null;
  const urgent = view?.rotation?.emergency || view?.restored_retired_at || (view?.cut_off.length ?? 0) > 0;
  return (
    <div className={styles.notice}>
      <Banner tone={urgent ? "bad" : "warn"}>
        <strong>{notice}</strong> <Link href={CERTIFICATE_AUTHORITY_PATH}>See the certificate authority</Link>
      </Banner>
    </div>
  );
}
