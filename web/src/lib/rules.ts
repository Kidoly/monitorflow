// Richer alert rules (ADR-0153): what each kind judges, in plain words, and
// the editor's form for it. The simple threshold stays in alerts.ts.

import { t } from "@/i18n";

import type {
  Comparison,
  EffectiveThreshold,
  RuleCondition,
  RuleFunction,
  RuleKind,
  ThresholdRequest,
  ThresholdSnapshot,
} from "./api";
import { type FormErrors, type Shape, type ThresholdForm, describeThreshold, thresholdRequest } from "./alerts";
import { formatNumber, formatPercent, formatSeconds } from "./format";
import { metricTitle } from "./metrics";
import { formatSensorValue } from "./sensors";

export const KINDS: RuleKind[] = ["value", "window", "compare", "forecast", "nodata"];
export const WINDOW_FUNCTIONS = ["avg", "min", "max", "change", "rate"] as const;
export const COMPARE_FUNCTIONS = ["ratio", "difference"] as const;
export const OFFSETS = [86_400, 604_800] as const;

/** The kind of a rule, `value` for servers older than ADR-0153. */
export const kindOf = (rule: { kind?: RuleKind | null }): RuleKind => rule.kind ?? "value";

/** The kinds the editor offers for a shape: an event only has its trigger and silence. */
export function kindsFor(shape: Shape): RuleKind[] {
  return shape.type === "event" ? ["value", "nodata"] : KINDS;
}

/** "The value", "When it will reach a value", read when shown. */
export const kindLabel = (kind: RuleKind) => t(`rules.kind.${kind}`);

const show = (shape: Shape, value: number) => {
  if (shape.type === "percent") return formatPercent(value);
  return shape.type === "number" && shape.unit ? `${formatNumber(value)} ${shape.unit}` : formatNumber(value);
};

const lower = (text: string) => text.charAt(0).toLocaleLowerCase() + text.slice(1);

/** The value of a rule's limit in the metric's words, for a condition. */
function conditionText(condition: RuleCondition): string {
  const title = metricTitle(condition.metric);
  const word = t(`checks.word.${condition.comparison}`);
  const value = formatSensorValue(condition.metric, condition.value);
  if (condition.function === "value" || !condition.window_s) return t("rules.describe.condition", { title, word, value });
  return t("rules.describe.conditionOver", {
    title,
    function: t(`rules.function.${condition.function}`),
    window: formatSeconds(condition.window_s),
    word,
    value,
  });
}

const overKey = (f: RuleFunction | null | undefined) =>
  f === "min" || f === "max" || f === "change" || f === "rate" ? f : "avg";

/** "Warn 3 days before a disk reaches 100% full, critical 1 day before." */
export function describeRule(shape: Shape, rule: EffectiveThreshold): string {
  const kind = kindOf(rule);
  const sentence = describeKind(kind, shape, rule);
  const conditions = rule.enabled ? (rule.conditions ?? []) : [];
  if (conditions.length === 0) return sentence;
  return t("rules.describe.and", {
    sentence,
    conditions: conditions.map(conditionText).join(t("rules.describe.conditionJoin")),
  });
}

function describeKind(kind: RuleKind, shape: Shape, rule: EffectiveThreshold): string {
  if (kind === "value" || !rule.enabled) return describeThreshold(shape, rule);
  const { noun } = shape;
  const suffix = shape.type === "event" ? "" : shape.suffix;
  const duration = rule.duration_s > 0 ? t("checks.describe.duration", { duration: formatSeconds(rule.duration_s) }) : "";
  const levels = (warn: (v: number) => string, crit: (v: number, also: boolean) => string) => {
    const parts: string[] = [];
    if (rule.warning !== null) parts.push(warn(rule.warning));
    if (rule.critical !== null) parts.push(crit(rule.critical, parts.length > 0));
    return parts.join(", ");
  };
  switch (kind) {
    case "window": {
      const numeric: Shape = shape.type === "event" ? { type: "number", noun, suffix: "" } : shape;
      const rest = lower(describeThreshold(numeric, rule));
      return t("rules.describe.window", {
        over: t(`rules.describe.over.${overKey(rule.function)}`, { window: formatSeconds(rule.window_s ?? 0) }),
        rest,
      });
    }
    case "compare": {
      const ratio = rule.function === "ratio";
      const value = (v: number) => (ratio ? formatNumber(v) : show(shape, v));
      const kindKey = ratio ? "ratio" : "difference";
      return t("rules.describe.compare", {
        when: t(`rules.when.${rule.offset_s === 86_400 ? 86_400 : 604_800}`),
        window: formatSeconds(rule.window_s ?? 0),
        levels: levels(
          (v) => t(`rules.level.${kindKey}.warn`, { value: value(v) }),
          (v) => t(`rules.level.${kindKey}.crit`, { value: value(v) }),
        ),
        duration,
      });
    }
    case "forecast": {
      const target = show(shape, rule.target ?? 0);
      return `${levels(
        (v) => t("rules.describe.forecast.warn", { time: formatSeconds(v), noun, target, suffix }),
        (v, also) =>
          also
            ? t("rules.describe.forecast.critAlso", { time: formatSeconds(v) })
            : t("rules.describe.forecast.crit", { time: formatSeconds(v), noun, target, suffix }),
      )}.`;
    }
    case "nodata":
      return `${levels(
        (v) => t("rules.describe.nodata.warn", { time: formatSeconds(v), noun }),
        (v, also) =>
          also ? t("rules.describe.nodata.critAlso", { time: formatSeconds(v) }) : t("rules.describe.nodata.crit", { time: formatSeconds(v), noun }),
      )}.`;
  }
}

/**
 * A value an incident or a preview judged, in the rule's words: the time
 * left of a forecast, a silence, a ratio, else the metric's value.
 */
export function judgedValue(
  metric: string,
  rule: Pick<ThresholdSnapshot, "kind" | "function"> | undefined,
  value: number,
): string {
  switch (rule?.kind ?? "value") {
    case "forecast":
      return value < 1 ? t("rules.value.reached") : t("rules.value.left", { time: formatSeconds(value) });
    case "nodata":
      return t("rules.value.silent", { time: formatSeconds(value) });
    case "compare":
      return rule?.function === "ratio"
        ? t("rules.value.times", { value: formatNumber(value) })
        : t("rules.value.more", { value: formatSensorValue(metric, value) });
    case "window":
      return rule?.function === "rate"
        ? t("rules.value.perSecond", { value: formatSensorValue(metric, value) })
        : formatSensorValue(metric, value);
    default:
      return formatSensorValue(metric, value);
  }
}

/** Whether warning and critical lines of `rule` are in the metric's unit, so a chart of it can draw them. */
export const linesInMetricUnit = (rule: Pick<ThresholdSnapshot, "kind" | "function">) => {
  const kind = rule.kind ?? "value";
  return kind === "value" || (kind === "window" && rule.function !== "rate" && rule.function !== "change");
};

/** One condition of the form, as typed. */
export interface ConditionForm {
  metric: string;
  comparison: Comparison;
  value: string;
}

/** The editor's form: the simple threshold's fields and those of the other kinds. */
export interface RuleForm extends ThresholdForm {
  kind: RuleKind;
  function: RuleFunction;
  /** Minutes, for a window or a comparison. */
  window: string;
  offset: number;
  /** In the shape's unit (a percentage for ratios). */
  target: string;
  /** Hours, for a forecast. */
  learn: string;
  conditions: ConditionForm[];
}

export type RuleErrors = FormErrors & Partial<Record<"window" | "target" | "learn" | "conditions", string>>;

const plain = (n: number) => String(Math.round(n * 1000) / 1000);
const isPercentMetric = (metric: string) => metric.endsWith("_ratio");

/** The form prefilled with `rule`, for `kind` (the rule's own by default). */
export function formOfRule(shape: Shape, rule: EffectiveThreshold, base: ThresholdForm, kind = kindOf(rule)): RuleForm {
  const own = kind === kindOf(rule);
  const percent = shape.type === "percent";
  const inShape = (v: number) => (percent ? plain(v * 100) : plain(v));
  const above = rule.comparison === ">" || rule.comparison === ">=";
  const defaults: RuleForm = {
    ...base,
    kind,
    function: kind === "compare" ? "ratio" : "avg",
    window: kind === "compare" ? "60" : "15",
    offset: 604_800,
    target: percent ? (above ? "100" : inShape(rule.critical ?? rule.warning ?? 0)) : inShape(rule.critical ?? rule.warning ?? 0),
    learn: "24",
    conditions: [],
  };
  if (kind === "forecast") Object.assign(defaults, { warning: "72", critical: "24", minutes: "0" });
  if (kind === "nodata") Object.assign(defaults, { warning: "10", critical: "", minutes: "0" });
  if (kind === "compare") Object.assign(defaults, { warning: "2", critical: "" });
  const conditions = (rule.conditions ?? []).map((c) => ({
    metric: c.metric,
    comparison: c.comparison,
    value: isPercentMetric(c.metric) ? plain(c.value * 100) : plain(c.value),
  }));
  if (!own) return { ...defaults, conditions: kind === "nodata" ? [] : conditions };
  const levels = (scale: number) => ({
    warning: rule.warning === null ? "" : plain(rule.warning / scale),
    critical: rule.critical === null ? "" : plain(rule.critical / scale),
  });
  switch (kind) {
    case "value":
      return { ...defaults, conditions };
    case "window":
      return { ...defaults, function: rule.function ?? "avg", window: plain((rule.window_s ?? 900) / 60), conditions };
    case "compare":
      return {
        ...defaults,
        ...(rule.function === "ratio" ? levels(1) : {}),
        function: rule.function ?? "ratio",
        window: plain((rule.window_s ?? 3600) / 60),
        offset: rule.offset_s ?? 604_800,
        conditions,
      };
    case "forecast":
      return {
        ...defaults,
        ...levels(3600),
        target: inShape(rule.target ?? 1),
        learn: plain((rule.window_s ?? 86_400) / 3600),
        conditions,
      };
    case "nodata":
      return { ...defaults, ...levels(60) };
  }
}

/** A number typed in any language: "1,5" reads as 1.5; null when empty. */
function read(text: string): number | null | undefined {
  const cleaned = text.trim().replace(/%$/, "").trim().replace(",", ".");
  if (cleaned === "") return null;
  const value = Number(cleaned);
  return Number.isFinite(value) ? value : undefined;
}

/** The request for `form`, or what is wrong with it, field by field. */
export function ruleRequest(
  shape: Shape,
  rule: EffectiveThreshold,
  form: RuleForm,
): { request: ThresholdRequest } | { errors: RuleErrors } {
  const errors: RuleErrors = {};
  const conditions: RuleCondition[] = [];
  for (const c of form.kind === "nodata" ? [] : form.conditions) {
    const value = read(c.value);
    if (value === null || value === undefined || c.metric === "") {
      errors.conditions = t("rules.error.number");
      continue;
    }
    conditions.push({
      metric: c.metric,
      instance: null,
      function: "value",
      comparison: c.comparison,
      value: isPercentMetric(c.metric) ? value / 100 : value,
    });
  }
  const finish = (request: ThresholdRequest) =>
    Object.keys(errors).length > 0 ? { errors } : { request: conditions.length ? { ...request, conditions } : request };

  if (form.kind === "value" || form.kind === "window") {
    const numeric: Shape = shape.type === "event" && form.kind === "window" ? { type: "number", noun: shape.noun, suffix: "" } : shape;
    const base = thresholdRequest(numeric, rule, form);
    if ("errors" in base) return { errors: { ...errors, ...base.errors } };
    // A plain threshold is sent as before ADR-0153, without a kind.
    if (form.kind === "value") return finish(base.request);
    const minutes = read(form.window);
    if (!minutes || minutes < 1 || minutes > 1440) errors.window = t("rules.error.window");
    const comparison = shape.type === "event" ? rule.comparison : base.request.comparison;
    return finish({
      ...base.request,
      comparison,
      kind: "window",
      function: form.function,
      window_s: Math.round((minutes ?? 0) * 60),
    });
  }

  const minutes = read(form.minutes) ?? 0;
  const common = {
    metric: rule.metric,
    instance: rule.instance,
    enabled: true,
    hysteresis: 0,
    duration_s: form.kind === "nodata" ? 0 : Math.round(Math.max(0, minutes) * 60),
  };
  const levels = (scale: number, low: number, high: number, message: string) => {
    const out: (number | null)[] = [];
    for (const key of ["warning", "critical"] as const) {
      const v = read(form[key]);
      if (v === undefined || (v !== null && (v * scale < low || v * scale > high))) {
        errors[key] = message;
        out.push(null);
      } else out.push(v === null ? null : v * scale);
    }
    if (out[0] === null && out[1] === null && !errors.warning && !errors.critical) errors.warning = t("checks.error.levels");
    return out as [number | null, number | null];
  };

  switch (form.kind) {
    case "compare": {
      const window = read(form.window);
      if (!window || window < 5 || window > 1440) errors.window = t("rules.error.compareWindow");
      const ratio = form.function === "ratio";
      const scale = ratio || shape.type !== "percent" ? 1 : 0.01;
      const [warning, critical] = levels(scale, -Infinity, Infinity, t("rules.error.number"));
      const above = rule.comparison === ">" || rule.comparison === ">=";
      if (warning !== null && critical !== null && (above ? critical < warning : critical > warning)) {
        errors.critical = above ? t("checks.error.criticalAbove") : t("checks.error.criticalBelow");
      }
      return finish({
        ...common,
        kind: "compare",
        comparison: shape.type === "event" ? ">" : rule.comparison,
        warning,
        critical,
        function: ratio ? "ratio" : "difference",
        window_s: Math.round((window ?? 0) * 60),
        offset_s: form.offset,
      });
    }
    case "forecast": {
      const learn = read(form.learn);
      if (!learn || learn < 1 || learn > 168) errors.learn = t("rules.error.learn");
      const target = read(form.target);
      if (target === null || target === undefined) errors.target = t("rules.error.target");
      const [warning, critical] = levels(3600, 60, 7_776_000, t("rules.error.before"));
      if (warning !== null && critical !== null && critical > warning) errors.critical = t("rules.error.sooner");
      const above = rule.comparison === ">" || rule.comparison === ">=";
      return finish({
        ...common,
        kind: "forecast",
        comparison: above ? ">=" : "<=",
        warning,
        critical,
        hysteresis: 3600,
        window_s: Math.round((learn ?? 0) * 3600),
        target: shape.type === "percent" ? (target ?? 0) / 100 : (target ?? 0),
      });
    }
    case "nodata": {
      const [warning, critical] = levels(60, 60, 604_800, t("rules.error.silence"));
      if (warning !== null && critical !== null && critical < warning) errors.critical = t("rules.error.longer");
      return finish({ ...common, kind: "nodata", comparison: ">", warning, critical });
    }
  }
}

/** The units of the level fields of a kind, beside the fields. */
export function levelUnit(shape: Shape, form: Pick<RuleForm, "kind" | "function">): string | undefined {
  switch (form.kind) {
    case "forecast":
      return t("format.unit.hour");
    case "nodata":
      return t("format.unit.minute");
    case "compare":
      if (form.function === "ratio") return t("rules.editor.times");
      break;
    default:
      break;
  }
  return shape.type === "percent" ? "%" : shape.type === "number" ? shape.unit : undefined;
}

/** The request that writes `rule` back as it is, turned on or off. */
export function requestOf(rule: EffectiveThreshold, enabled: boolean): ThresholdRequest {
  const kind = kindOf(rule);
  return {
    metric: rule.metric,
    instance: rule.instance,
    ...(kind !== "value" && { kind }),
    comparison: rule.comparison,
    warning: rule.warning,
    critical: rule.critical,
    duration_s: rule.duration_s,
    hysteresis: rule.hysteresis,
    enabled,
    ...(rule.function && { function: rule.function }),
    ...(rule.window_s && { window_s: rule.window_s }),
    ...(rule.offset_s && { offset_s: rule.offset_s }),
    ...(rule.target !== null && rule.target !== undefined && { target: rule.target }),
    ...(rule.conditions?.length && { conditions: rule.conditions }),
  };
}

/** An incident's value in the words of the rule that opened it. */
export function incidentValue(
  incident: { metric: string; kind?: RuleKind; threshold: Pick<ThresholdSnapshot, "kind" | "function"> },
  value: number,
): string {
  return judgedValue(incident.metric, { kind: incident.kind ?? incident.threshold.kind, function: incident.threshold.function }, value);
}
