import type { Metadata } from "next";

import { ScriptLibrary } from "@/components/ScriptLibrary";

export const metadata: Metadata = { title: "Scripts · MonitorFlow" };

export default function ScriptsPage() {
  return <ScriptLibrary />;
}
