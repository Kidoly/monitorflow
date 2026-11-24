import { t } from "@/i18n";

import type {
  AutomationEvent,
  Role,
  ScriptInterpreter,
  ScriptParameter,
  ScriptParameterType,
  ScriptRun,
  ScriptRunState,
  ScriptTarget,
} from "./api";

/** The script library (ADR-0189): what the forms hold and send. */

export const INTERPRETERS: ScriptInterpreter[] = ["sh", "powershell"];
export const RUN_ROLES: Role[] = ["operator", "approver", "administrator"];
export const PARAMETER_TYPES: ScriptParameterType[] = ["string", "integer", "boolean", "choice"];
export const EVENTS: AutomationEvent[] = ["incident_opened", "check_failed", "server_joined"];
export const RUN_STATES: ScriptRunState[] = [
  "awaiting_approval",
  "scheduled",
  "dispatched",
  "rejected",
  "expired",
  "cancelled",
  "refused",
];

/** A parameter as the editor holds it: choices one per line, the default as text. */
export interface ParameterDraft {
  name: string;
  label: string;
  type: ScriptParameterType;
  required: boolean;
  defaultValue: string;
  choices: string;
}

export function parameterDraft(p: ScriptParameter): ParameterDraft {
  return {
    name: p.name,
    label: p.label,
    type: p.type,
    required: p.required,
    defaultValue: p.default ?? "",
    choices: p.choices.join("\n"),
  };
}

export const NEW_PARAMETER: ParameterDraft = {
  name: "",
  label: "",
  type: "string",
  required: false,
  defaultValue: "",
  choices: "",
};

const NAME = /^[a-z][a-z0-9_]{0,31}$/;

/** The parameters as the API takes them, or why not. The API checks again. */
export function parametersOf(drafts: ParameterDraft[]): ScriptParameter[] | string {
  const parameters: ScriptParameter[] = [];
  for (const draft of drafts) {
    const name = draft.name.trim();
    if (!NAME.test(name)) return t("scripts.form.badParameterName", { name: name || "?" });
    if (parameters.some((p) => p.name === name)) return t("scripts.form.parameterTwice", { name });
    const choices = draft.type === "choice" ? draft.choices.split("\n").map((c) => c.trim()).filter(Boolean) : [];
    if (draft.type === "choice" && choices.length === 0) return t("scripts.form.noChoices", { name });
    const defaultValue = draft.defaultValue.trim();
    if (defaultValue) {
      const why = valueProblem({ name, label: "", type: draft.type, required: false, default: null, choices }, defaultValue);
      if (why) return why;
    }
    parameters.push({
      name,
      label: draft.label.trim(),
      type: draft.type,
      required: draft.required,
      default: defaultValue || null,
      choices,
    });
  }
  return parameters;
}

/** Why `value` does not suit `parameter`, or null. */
export function valueProblem(parameter: ScriptParameter, value: string): string | null {
  const name = parameter.name;
  if (value.length > 256 || /[\u0000-\u001f\u007f]/.test(value)) return t("scripts.run.badLine", { name });
  switch (parameter.type) {
    case "integer":
      return /^[+-]?\d+$/.test(value) ? null : t("scripts.run.badInteger", { name });
    case "boolean":
      return value === "true" || value === "false" ? null : t("scripts.run.badBoolean", { name });
    case "choice":
      return parameter.choices.includes(value) ? null : t("scripts.run.badChoice", { name });
    default:
      return null;
  }
}

/** The values a run sends, empty ones left to the defaults, or why not. */
export function valuesOf(parameters: ScriptParameter[], given: Record<string, string>): Record<string, string> | string {
  const values: Record<string, string> = {};
  for (const parameter of parameters) {
    const value = (given[parameter.name] ?? "").trim();
    if (!value) {
      if (parameter.required && parameter.default === null) return t("scripts.run.missing", { name: parameter.label || parameter.name });
      continue;
    }
    const why = valueProblem(parameter, value);
    if (why) return why;
    values[parameter.name] = value;
  }
  return values;
}

/** The environment variable a parameter reaches the script as. */
export function envName(name: string): string {
  return `MF_PARAM_${name.toUpperCase()}`;
}

export type TargetKind = ScriptTarget["kind"];

/** The target the run form chose, or why it cannot run. */
export function targetOf(kind: TargetKind, choice: { servers: string[]; site: string; service: string }): ScriptTarget | string {
  switch (kind) {
    case "servers":
      return choice.servers.length ? { kind, server_ids: choice.servers } : t("scripts.run.noServer");
    case "site":
      return choice.site ? { kind, site_id: choice.site } : t("scripts.run.noSite");
    case "business_service":
      return choice.service ? { kind, business_service_id: choice.service } : t("scripts.run.noService");
  }
}

/** A `datetime-local` value as an ISO time, or undefined for now. */
export function scheduledAt(local: string): string | undefined {
  if (!local) return undefined;
  const at = new Date(local);
  return Number.isNaN(at.getTime()) ? undefined : at.toISOString();
}

/** Where a run is going, in words. */
export function runProgress(run: Pick<ScriptRun, "state" | "counts">): string {
  const { servers, pending, succeeded, failed } = run.counts;
  if (run.state !== "dispatched") return t("scripts.runs.servers", { count: servers });
  if (pending > 0) return t("scripts.runs.progress", { done: succeeded + failed, count: servers });
  return failed > 0 ? t("scripts.runs.failedOf", { failed, count: servers }) : t("scripts.runs.allSucceeded", { count: servers });
}

/** The status words of a run, for `StatusLabel kind="scriptRun"`. */
export function runStatus(run: Pick<ScriptRun, "state" | "counts">): string {
  if (run.state !== "dispatched") return run.state;
  if (run.counts.pending > 0) return "running";
  return run.counts.failed > 0 ? "failed" : "succeeded";
}
