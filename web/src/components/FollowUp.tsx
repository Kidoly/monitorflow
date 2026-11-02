"use client";

import { type FormEvent, useCallback, useState } from "react";

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
import { useMe } from "@/lib/me";
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
  const named = (policy?.steps ?? []).flatMap((s) => s.users).map((u) => ({ id: u.id, name: u.name ?? "a removed user" }));
  const people = [
    ...(candidates?.people ?? []).map((p) => ({ id: p.id, name: p.has_contact ? p.username : `${p.username} (no email or chat yet)` })),
    ...named.filter((n) => !(candidates?.people ?? []).some((p) => p.id === n.id)),
  ].filter((p, i, all) => all.findIndex((q) => q.id === p.id) === i);

  const change = (index: number, patch: Partial<FollowUpStep>) =>
    setSteps(steps.map((s, i) => (i === index ? { ...s, ...patch } : s)));

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!chosen) return setError("Choose what this follow-up is for.");
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
    <form className={styles.form} onSubmit={(e) => void submit(e)} aria-label={policy ? "Edit follow-up" : "New follow-up"}>
      {scopes.length > 1 && (
        <label className={admin.field}>
          Follow-up for
          <select className={fieldClass} required value={chosen?.value ?? ""} disabled={Boolean(policy)} onChange={(e) => setScope(e.target.value)}>
            <option value="" disabled>
              Choose…
            </option>
            {scopes.map((s) => (
              <option key={s.value} value={s.value}>
                {s.label}
              </option>
            ))}
          </select>
          <span className={admin.fieldNote}>Replaces any follow-up already set for this place.</span>
        </label>
      )}
      {steps.length === 0 && <p className={admin.dim}>No steps: nobody is told after the first message.</p>}
      <ol className={styles.steps}>
        {steps.map((step, i) => (
          <li key={i} aria-label={`Step ${i + 1}`}>
            <div className={styles.stepHead}>
              <strong>Step {i + 1}</strong>
              <label className={styles.inline}>
                after{" "}
                <input
                  className={fieldClass}
                  type="number"
                  min={1}
                  max={1440}
                  required
                  aria-label={`Minutes before step ${i + 1}`}
                  value={step.after_minutes}
                  onChange={(e) => change(i, { after_minutes: Number(e.target.value) })}
                />{" "}
                minutes without acknowledgement{i > 0 ? " since the step before" : ""}, tell:
              </label>
              <Button size="small" disabled={i === 0} aria-label={`Move step ${i + 1} up`} onClick={() => setSteps(move(steps, i, -1))}>
                ↑
              </Button>
              <Button size="small" disabled={i === steps.length - 1} aria-label={`Move step ${i + 1} down`} onClick={() => setSteps(move(steps, i, 1))}>
                ↓
              </Button>
              <Button size="small" aria-label={`Remove step ${i + 1}`} onClick={() => setSteps(steps.filter((_, j) => j !== i))}>
                Remove
              </Button>
            </div>
            <label>
              <input type="checkbox" checked={step.administrators} onChange={(e) => change(i, { administrators: e.target.checked })} /> The
              administrators of the place
            </label>
            <Checks
              legend="The person on call of"
              options={list.schedules.filter(fits)}
              chosen={step.on_call}
              onChange={(on_call) => change(i, { on_call })}
            />
            <Checks legend="People" options={chosen ? people : []} chosen={step.users} onChange={(users) => change(i, { users })} />
            <Checks legend="Channels" options={list.channels.filter(fits)} chosen={step.channels} onChange={(channels) => change(i, { channels })} />
          </li>
        ))}
      </ol>
      <div>
        <Button size="small" disabled={steps.length >= 10} onClick={() => setSteps([...steps, { ...NEW_STEP, administrators: false }])}>
          Add a step
        </Button>
      </div>
      <div className={admin.formFoot}>
        <Button type="submit" variant="k" disabled={busy}>
          {busy ? "Saving…" : "Save"}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          Cancel
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
  if (policy.steps.length === 0) return <p className={admin.dim}>Nobody is told after the first message.</p>;
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
  const { data, error, refresh } = usePolledData(loadPolicies, { enabled });
  const [editing, setEditing] = useState<FollowUpPolicy | null | undefined>(undefined);
  if (!enabled || (error instanceof ApiError && error.status === 403)) return null;
  const several = new Set((data?.policies ?? []).map((p) => p.customer_id).filter(Boolean)).size > 1;
  const wait = data?.default_steps[0]?.after_minutes ?? 15;
  return (
    <section className={admin.section} aria-labelledby="follow-up-title">
      <div className={admin.sectionHead}>
        <span id="follow-up-title" className={admin.sectionTitle}>
          If nobody acknowledges
        </span>
        <span className={admin.dim}>Acknowledging an alert says someone is on it. Until then, the follow-up tells more people, step by step.</span>
      </div>
      <div className={styles.body}>
        {!data && error !== null ? (
          <p role="alert">{describeError(error)}</p>
        ) : data ? (
          <>
            <div>
              <strong>Default</strong>
              <p className={styles.stepText}>
                Critical alerts not acknowledged within {minutesText(wait)} go to the nearest administrators of the place, in person.
                Warnings are not followed up.
              </p>
            </div>
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
                <div key={p.id} aria-label={`Follow-up for ${placeName(p, several)}`} role="group">
                  <strong>{placeName(p, several)}</strong>
                  <Steps policy={p} />
                  <div className={styles.actions}>
                    <Button size="small" onClick={() => setEditing(p)}>
                      Edit
                    </Button>
                    <InlineConfirm
                      trigger="Back to the default…"
                      triggerVariant="default"
                      title={`Use the default for ${placeName(p, several)}?`}
                      consequence={`Critical alerts nobody acknowledges go to the nearest administrators after ${minutesText(wait)}.`}
                      confirmLabel="Use the default"
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
                <Button onClick={() => setEditing(null)}>Set a follow-up…</Button>
              </div>
            )}
          </>
        ) : null}
      </div>
    </section>
  );
}
