"use client";

import { useRouter } from "next/navigation";
import { useEffect } from "react";

import { LoadingBlock } from "@/components/ui/PageState";
import { LEGACY_SETTINGS_PATHS, movedSettingsHref } from "@/lib/routes";

/**
 * The page left at a settings page's old address: it sends the browser to
 * the new one, query and hash kept, without adding to its history. A static
 * export has no server redirects, so the client does it.
 */
export function MovedPage({ from }: { from: keyof typeof LEGACY_SETTINGS_PATHS }) {
  const router = useRouter();
  useEffect(() => {
    const { search, hash } = window.location;
    router.replace(movedSettingsHref(from, search, hash) ?? "/");
  }, [from, router]);
  return <LoadingBlock label="Opening Settings" />;
}
