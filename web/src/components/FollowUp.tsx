"use client";

import { type FormEvent, useCallback, useState } from "react";

import { rich, t } from "@/i18n";
import {
  ApiError,
  type FollowUpChoice,
  type FollowUpPolicy,
  type FollowUpPolicyList,
  type FollowUpStep,
  type OnCallPerson,
  api,
  describeError,
} from "@/lib/api";
import { covers, minutesText, stepRequest, stepSentence } from "@/lib/followUp";
import { useMe, useMultiCustomer } from "@/lib/me";
import { channelScopes, scopeValue } from "@/lib/notifications";
import { move, placeName } from "@/lib/onCall";
import { usePolledData } from "@/lib/usePolledData";
import { Button, fieldClass } from "@/components/ui/Button";
import { InlineConfirm } from "@/components/ui/InlineConfirm";

import admin from "./AdminPages.module.css";
import styles from "./OnCall.module.css";

const loadPolicies = (signal: AbortSignal) => api.followUpPolicies(signal);

async function loadPlaces(signal: AbortSignal) {
  const [{ customers }, { sites }] = await Promise.all([api.customers(signal), api.sites(signal)]);
  return { customers, sites };
}

const NEW_STEP: FollowUpStep = { after_minutes: 15, on_call: [], users: [], channels: [], administrators: true };

/** "In 15 minutes": a step's checkboxes, one per schedule, person or channel. */
function Checks({
  legend,
  options,
  chosen,
  onChange,
}: {
  legend: string;
  options: { id: string; name: string }[];
  chosen: string[];
  onChange: (ids: string[]) => void;
}) {
  if (options.length === 0) return null;
  return (
    <fieldset className={styles.checks}>
      <legend>{legend}</legend>
      {options.map((o) => (
        <label key={o.id}>
          <input
            type="checkbox"
            checked={chosen.includes(o.id)}
            onChange={(e) => onChange(e.target.checked ? [...chosen, o.id] : chosen.filter((id) => id !== o.id))}
          />{" "}
          {o.name}
        </label>
      ))}
    </fieldset>
  );
}

/** Setting the steps of one place: who is told, after how long without acknowledgement. */
function FollowUpForm({
  policy,
  list,
  onSaved,
  onCancel,
}: {
  policy?: FollowUpPolicy;
  list: FollowUpPolicyList;
  onSaved: () => void;
  onCancel: () => void;
}) {
  const me = useMe();
  const { data: places } = usePolledData(loadPlaces);
  const scopes = channelScopes(me, places?.customers ?? [], places?.sites ?? [], policy);
  const [scope, setScope] = useState(policy ? scopeValue(policy) : "");
  const chosen = scopes.find((s) => s.value === scope) ?? (scopes.length === 1 ? scopes[0] : undefined);
  const [steps, setSteps] = useState<FollowUpStep[]>(policy ? policy.steps.map(stepRequest) : [{ ...NEW_STEP }]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const peopleFetcher = useCallback(
    (signal: AbortSignal): Promise<{ people: OnCallPerson[] }> =>
      chosen ? api.onCallPeople({ customer_id: chosen.customer_id, site_id: chosen.site_id }, signal) : Promise.resolve({ people: [] }),
    // Only a change of place asks again.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [chosen?.value],
  );
  const { data: candidates } = usePolledData(peopleFetcher);
  const siteCustomer = new Map((places?.sites ?? []).map((s) => [s.id, s.customer_id]));
  const fits = (c: FollowUpChoice) => (chosen ? covers(c, chosen, siteCustomer) : false);
  // People already named stay choices, though they could not be added now.
  const named = (policy?.steps ?? []).flatMap((s) => s.users).map((u) => ({ id: u.id, name: u.name ?? t("onCall.removedUser") }));
  const people = [
    ...(candidates?.people ?? []).map((p) => ({ id: p.id, name: p.has_contact ? p.username : t("onCall.noContactYet", { name: p.username }) })),
    ...named.filter((n) => !(candidates?.people ?? []).some((p) => p.id === n.id)),
  ].filter((p, i, all) => all.findIndex((q) => q.id === p.id) === i);

  const change = (index: number, patch: Partial<FollowUpStep>) =>
    setSteps(steps.map((s, i) => (i === index ? { ...s, ...patch } : s)));

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!chosen) return setError(t("followUp.missingPlace"));
    setBusy(true);
    setError(null);
    try {
      await api.saveFollowUpPolicy({ customer_id: chosen.customer_id, site_id: chosen.site_id, steps });
      onSaved();
    } catch (e) {
      setError(e instanceof ApiError && e.status === 400 ? e.message : describeError(e));
      setBusy(false);
    }
  }

  return (
    <form className={styles.form} onSubmit={(e) => void submit(e)} aria-label={policy ? t("followUp.edit") : t("followUp.new")}>
      {scopes.length > 1 && (
        <label className={admin.field}>
          {t("followUp.for")}
          <select className={fieldClass} required value={chosen?.value ?? ""} disabled={Boolean(policy)} onChange={(e) => setScope(e.target.value)}>
            <option value="" disabled>
              {t("onCall.choose")}
            </option>
            {scopes.map((s) => (
              <option key={s.value} value={s.value}>
                {s.label}
              </option>
            ))}
          </select>
          <span className={admin.fieldNote}>{t("followUp.forNote")}</span>
        </label>
      )}
      {steps.length === 0 && <p className={admin.dim}>{t("followUp.noSteps")}</p>}
      <ol className={styles.steps}>
        {steps.map((step, i) => (
          <li key={i} aria-label={t("followUp.stepName", { number: i + 1 })}>
            <div className={styles.stepHead}>
              <strong>{t("followUp.stepName", { number: i + 1 })}</strong>
              <label className={styles.inline}>
                {rich(i > 0 ? "followUp.afterPrevious" : "followUp.after", {
                  minutes: (
                    <input
                      className={fieldClass}
                      type="number"
                      min={1}
                      max={1440}
                      required
                      aria-label={t("followUp.minutesBefore", { number: i + 1 })}
                      value={step.after_minutes}
                      onChange={(e) => change(i, { after_minutes: Number(e.target.value) })}
                    />
                  ),
                })}
              </label>
              <Button size="small" disabled={i === 0} aria-label={t("followUp.up", { number: i + 1 })} onClick={() => setSteps(move(steps, i, -1))}>
                ↑
              </Button>
              <Button
                size="small"
                disabled={i === steps.length - 1}
                aria-label={t("followUp.down", { number: i + 1 })}
                onClick={() => setSteps(move(steps, i, 1))}
              >
                ↓
              </Button>
              <Button size="small" aria-label={t("followUp.removeStep", { number: i + 1 })} onClick={() => setSteps(steps.filter((_, j) => j !== i))}>
                {t("followUp.remove")}
              </Button>
            </div>
            <label>
              <input type="checkbox" checked={step.administrators} onChange={(e) => change(i, { administrators: e.target.checked })} />{" "}
              {t("followUp.administratorsLabel")}
            </label>
            <Checks
              legend={t("followUp.personOnCallOf")}
              options={list.schedules.filter(fits)}
              chosen={step.on_call}
              onChange={(on_call) => change(i, { on_call })}
            />
            <Checks legend={t("followUp.people")} options={chosen ? people : []} chosen={step.users} onChange={(users) => change(i, { users })} />
            <Checks legend={t("followUp.channels")} options={list.channels.filter(fits)} chosen={step.channels} onChange={(channels) => change(i, { channels })} />
          </li>
        ))}
      </ol>
      <div>
        <Button size="small" disabled={steps.length >= 10} onClick={() => setSteps([...steps, { ...NEW_STEP, administrators: false }])}>
          {t("followUp.addStep")}
        </Button>
      </div>
      <div className={admin.formFoot}>
        <Button type="submit" variant="k" disabled={busy}>
          {busy ? t("onCall.saving") : t("onCall.save")}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          {t("onCall.cancel")}
        </Button>
        {error && (
          <p className={admin.formError} role="alert">
            {error}
          </p>
        )}
      </div>
    </form>
  );
}

function Steps({ policy }: { policy: Pick<FollowUpPolicy, "steps"> }) {
  if (policy.steps.length === 0) return <p className={admin.dim}>{t("followUp.nobodyAfter")}</p>;
  return (
    <ol className={styles.stepList}>
      {policy.steps.map((s, i) => (
        <li key={i}>{stepSentence(s)}</li>
      ))}
    </ol>
  );
}

/**
 * "If nobody acknowledges" (ADR-0082): who is told next, per place, for
 * administrators. Others get 403 from the API and see nothing.
 */
export function FollowUpSection({ enabled }: { enabled: boolean }) {
  const multi = useMultiCustomer();
  const { data, error, refresh } = usePolledData(loadPolicies, { enabled });
  const [editing, setEditing] = useState<FollowUpPolicy | null | undefined>(undefined);
  if (!enabled || (error instanceof ApiError && error.status === 403)) return null;
  const several = multi && new Set((data?.policies ?? []).map((p) => p.customer_id).filter(Boolean)).size > 1;
  const wait = data?.default_steps[0]?.after_minutes ?? 15;
  return (
    <section className={admin.section} aria-labelledby="follow-up-title">
      <div className={admin.sectionHead}>
        <span id="follow-up-title" className={admin.sectionTitle}>
          {t("followUp.title")}
        </span>
        <span className={admin.dim}>{t("followUp.intro")}</span>
      </div>
      <div className={styles.body}>
        {!data && error !== null ? (
          <p role="alert">{describeError(error)}</p>
        ) : data ? (
          <>
            {/* A new follow-up's form starts from the default and shows it. */}
            {editing !== null && (
              <div>
                <strong>{t("followUp.default")}</strong>
                <p className={styles.stepText}>{t("followUp.defaultText", { minutes: minutesText(wait) })}</p>
              </div>
            )}
            {data.policies.map((p) =>
              editing?.id === p.id ? (
                <FollowUpForm
                  key={p.id}
                  policy={p}
                  list={data}
                  onCancel={() => setEditing(undefined)}
                  onSaved={() => {
                    setEditing(undefined);
                    refresh();
                  }}
                />
              ) : (
                <div key={p.id} aria-label={t("followUp.forPlace", { place: placeName(p, several, multi) })} role="group">
                  <strong>{placeName(p, several, multi)}</strong>
                  <Steps policy={p} />
                  <div className={styles.actions}>
                    <Button size="small" onClick={() => setEditing(p)}>
                      {t("onCall.edit")}
                    </Button>
                    <InlineConfirm
                      trigger={t("followUp.backToDefault")}
                      triggerVariant="default"
                      title={t("followUp.useDefault.title", { place: placeName(p, several, multi) })}
                      consequence={t("followUp.useDefault.consequence", { minutes: minutesText(wait) })}
                      confirmLabel={t("followUp.useDefault.confirm")}
                      onConfirm={async () => {
                        await api.deleteFollowUpPolicy(p.id);
                        refresh();
                      }}
                      describeError={describeError}
                    />
                  </div>
                </div>
              ),
            )}
            {editing === null ? (
              <FollowUpForm
                list={data}
                onCancel={() => setEditing(undefined)}
                onSaved={() => {
                  setEditing(undefined);
                  refresh();
                }}
              />
            ) : (
              <div>
                <Button onClick={() => setEditing(null)}>{t("followUp.set")}</Button>
              </div>
            )}
          </>
        ) : null}
      </div>
    </section>
  );
}
