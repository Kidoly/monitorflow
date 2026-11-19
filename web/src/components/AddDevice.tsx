"use client";

import { useRouter } from "next/navigation";
import { type FormEvent, useEffect, useState } from "react";

import { t } from "@/i18n";
import {
  type Detected,
  type Probe,
  type Site,
  type SnmpCredentialsRequest,
  type SnmpTemplate,
  type VmwareCredentialsRequest,
  type VmwareTested,
  api,
  describeError,
} from "@/lib/api";
import { placeLabel } from "@/lib/customers";
import { modifiesSite, multiCustomer, multiSite, useMe } from "@/lib/me";
import { serverHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { Breadcrumb } from "@/components/ui/Breadcrumb";
import { Button, fieldClass } from "@/components/ui/Button";
import { Page } from "@/components/ui/Page";
import { PageHeader } from "@/components/ui/PageState";

import styles from "./AdminPages.module.css";
import { NEW_CREDENTIALS, SnmpFields, snmpProblem, snmpRequest } from "./SnmpCredentials";
import { NEW_VMWARE, VmwareFields, VmwareTest, needsTrust, vmwareProblem, vmwareRequest } from "./VmwareCredentials";

/** An IP address (v4, or v6 with or without brackets) or a DNS name, as the server checks it. */
export function validAddress(value: string): boolean {
  const bare = value.trim().replace(/^\[(.*)\]$/, "$1");
  if (bare === "" || bare.length > 253) return false;
  if (/^[0-9a-f:.]+$/i.test(bare) && bare.includes(":")) return true;
  return bare.split(".").every((label) => /^[A-Za-z0-9_][A-Za-z0-9_-]*$/.test(label));
}

/** "The central server", "Relay paris-1", with "(recommended)" on the default. */
export function probeLabel(probe: Probe): string {
  const name =
    probe.probe === "central"
      ? t("devices.from.central")
      : probe.online
        ? t("devices.from.relay", { name: probe.name ?? probe.key })
        : t("devices.from.relayOffline", { name: probe.name ?? probe.key });
  return probe.default ? t("devices.from.default", { probe: name }) : name;
}

/** The value of a probe in a `probe` field: `central` or the relay's ID. */
export const probeValue = (probe: Probe) => probe.relay_id ?? "central";

/** Where a site's checks can run from, the default first chosen; empty while loading. */
export function useProbes(siteId: string): Probe[] {
  const [probes, setProbes] = useState<Probe[]>([]);
  useEffect(() => {
    if (!siteId) return;
    const controller = new AbortController();
    api
      .siteProbes(siteId, controller.signal)
      .then((list) => setProbes(list.probes))
      .catch(() => {
        if (!controller.signal.aborted) setProbes([]);
      });
    return () => controller.abort();
  }, [siteId]);
  return probes;
}

/** The "Checked from" choice of a site. */
export function ProbeSelect({
  siteId,
  value,
  onChange,
  label,
}: {
  siteId: string;
  value: string;
  onChange: (value: string) => void;
  label: string;
}) {
  const probes = useProbes(siteId);
  const fallback = probes.find((p) => p.default);
  // Nothing chosen yet: the default is.
  useEffect(() => {
    if (!value && fallback) onChange(probeValue(fallback));
  }, [value, fallback, onChange]);
  return (
    <label className={styles.field}>
      {label}
      <select className={fieldClass} value={value} onChange={(e) => onChange(e.target.value)} disabled={probes.length === 0}>
        {probes.map((probe) => (
          <option key={probeValue(probe)} value={probeValue(probe)}>
            {probeLabel(probe)}
          </option>
        ))}
      </select>
    </label>
  );
}

/** Fleet › Add a device (ADR-0148). */
export function AddDevicePage() {
  const scope = useScope();
  return (
    <Page>
      <Breadcrumb parents={[{ label: t("server.fleet"), href: scopedHref("/", scope) }]} />
      <PageHeader title={t("devices.add.title")} />
      <AddDevice />
    </Page>
  );
}

export function AddDevice() {
  const me = useMe();
  const router = useRouter();
  const scope = useScope();
  const [sites, setSites] = useState<Site[] | null>(null);
  const [siteId, setSiteId] = useState(scope.siteId ?? "");
  const [name, setName] = useState("");
  const [address, setAddress] = useState("");
  const [probe, setProbe] = useState("");
  const [ping, setPing] = useState(true);
  const [snmp, setSnmp] = useState(false);
  const [credentials, setCredentials] = useState<SnmpCredentialsRequest>(NEW_CREDENTIALS);
  const [templates, setTemplates] = useState<SnmpTemplate[]>([]);
  const [template, setTemplate] = useState("");
  const [detected, setDetected] = useState<Detected | null>(null);
  const [detecting, setDetecting] = useState(false);
  const [snmpError, setSnmpError] = useState<string | null>(null);
  const [vmware, setVmware] = useState(false);
  const [vcenter, setVcenter] = useState<VmwareCredentialsRequest>(NEW_VMWARE);
  const [vcenterTested, setVcenterTested] = useState<VmwareTested | null>(null);
  const [vcenterError, setVcenterError] = useState<string | null>(null);
  const [errors, setErrors] = useState<{ name?: string; address?: string }>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    api
      .sites(controller.signal)
      .then(({ sites }) => {
        const usable = sites.filter((site) => modifiesSite(me, site.customer_id, site.id));
        setSites(usable);
        setSiteId((current) => (usable.some((s) => s.id === current) ? current : (usable[0]?.id ?? "")));
      })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setError(t("devices.add.loadFailed", { error: describeError(e) }));
      });
    return () => controller.abort();
  }, [me]);

  useEffect(() => {
    if (!snmp || templates.length > 0) return;
    const controller = new AbortController();
    api
      .deviceTemplates(controller.signal)
      .then((list) => setTemplates(list.templates))
      .catch(() => {});
    return () => controller.abort();
  }, [snmp, templates.length]);

  async function detect() {
    const problem = (!validAddress(address) && t("devices.error.address")) || snmpProblem(credentials, false);
    setSnmpError(problem || null);
    if (problem) return;
    setDetecting(true);
    setDetected(null);
    try {
      const result = await api.detectDevice({
        site_id: siteId,
        address: address.trim(),
        snmp: snmpRequest(credentials),
        ...(probe && { probe }),
      });
      setDetected(result);
      if (result.template) setTemplate(result.template);
      if (result.name && !name.trim()) setName(result.name);
    } catch (e) {
      setSnmpError(t("devices.snmp.detectFailed", { error: describeError(e) }));
    } finally {
      setDetecting(false);
    }
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    const found = {
      ...(name.trim() === "" && { name: t("devices.error.name") }),
      ...(!validAddress(address) && { address: t("devices.error.address") }),
    };
    setErrors(found);
    if (Object.keys(found).length > 0) return;
    if (snmp) {
      const problem = snmpProblem(credentials, false) ?? (template ? null : t("devices.snmp.needTemplate"));
      setSnmpError(problem);
      if (problem) return;
    }
    if (vmware) {
      const problem = vmwareProblem(vcenter, false) ?? (needsTrust(vcenterTested, vcenter) ? t("virtualization.vmware.needTrust") : null);
      setVcenterError(problem);
      if (problem) return;
    }
    setBusy(true);
    setError(null);
    try {
      const device = await api.createDevice({
        site_id: siteId,
        name: name.trim(),
        address: address.trim(),
        ping,
        ...((ping || snmp || vmware) && probe && { probe }),
        ...(snmp && { snmp: snmpRequest(credentials), template }),
        ...(vmware && { vmware: vmwareRequest(vcenter) }),
      });
      router.push(scopedHref(serverHref(device.id), scope));
    } catch (e) {
      setError(t("devices.add.failed", { error: describeError(e) }));
      setBusy(false);
    }
  }

  if (sites?.length === 0) return <p className={styles.sectionNote}>{t("devices.add.noSite")}</p>;
  return (
    <form className={styles.panel} onSubmit={(e) => void submit(e)} aria-label={t("devices.add.title")} noValidate>
      <p className={styles.sectionNote}>{t("devices.add.note")}</p>
      <label className={styles.field}>
        {t("devices.add.name")}
        <input
          className={fieldClass}
          value={name}
          placeholder={t("devices.add.namePlaceholder")}
          onChange={(e) => setName(e.target.value)}
          aria-invalid={errors.name ? true : undefined}
        />
        {errors.name && <span role="alert" className={styles.formError}>{errors.name}</span>}
      </label>
      <label className={styles.field}>
        {t("devices.add.address")}
        <input
          className={fieldClass}
          value={address}
          placeholder={t("devices.add.addressPlaceholder")}
          spellCheck={false}
          autoCapitalize="none"
          onChange={(e) => setAddress(e.target.value)}
          aria-invalid={errors.address ? true : undefined}
        />
        {errors.address ? (
          <span role="alert" className={styles.formError}>{errors.address}</span>
        ) : (
          <span className={styles.sectionNote}>{t("devices.add.addressHint")}</span>
        )}
      </label>
      {multiSite(me) && sites && (
        <label className={styles.field}>
          {t("devices.add.site")}
          <select
            className={fieldClass}
            value={siteId}
            onChange={(e) => {
              setSiteId(e.target.value);
              setProbe("");
            }}
          >
            {sites.map((site) => (
              <option key={site.id} value={site.id}>
                {placeLabel(site.customer_name, site.name, multiCustomer(me))}
              </option>
            ))}
          </select>
        </label>
      )}
      <label className={styles.check}>
        <input type="checkbox" checked={ping} onChange={(e) => setPing(e.target.checked)} /> {t("devices.add.ping")}
      </label>
      <label className={styles.check}>
        <input type="checkbox" checked={snmp} onChange={(e) => setSnmp(e.target.checked)} /> {t("devices.snmp.watch")}
      </label>
      <label className={styles.check}>
        <input type="checkbox" checked={vmware} onChange={(e) => setVmware(e.target.checked)} /> {t("virtualization.vmware.watch")}
      </label>
      {(ping || snmp || vmware) && siteId && (
        <ProbeSelect key={siteId} siteId={siteId} value={probe} onChange={setProbe} label={t("devices.add.from")} />
      )}
      {snmp && (
        <fieldset className={styles.panel} aria-label={t("devices.snmp.title")}>
          <SnmpFields value={credentials} onChange={setCredentials} />
          <div className={styles.formFoot}>
            <Button onClick={() => void detect()} disabled={detecting || !siteId}>
              {detecting ? t("devices.snmp.detecting") : t("devices.snmp.detect")}
            </Button>
          </div>
          {detected && <DetectedLine detected={detected} />}
          <label className={styles.field}>
            {t("devices.snmp.template")}
            <select className={fieldClass} value={template} onChange={(e) => setTemplate(e.target.value)}>
              <option value="">{"–"}</option>
              {templates.map((tpl) => (
                <option key={tpl.key} value={tpl.key}>
                  {t(`devices.template.${tpl.key as "switch"}`)}
                </option>
              ))}
            </select>
            <span className={styles.sectionNote}>{t("devices.snmp.templateHint")}</span>
          </label>
          {snmpError && (
            <p className={styles.formError} role="alert">
              {snmpError}
            </p>
          )}
        </fieldset>
      )}
      {vmware && (
        <fieldset className={styles.panel} aria-label={t("virtualization.vmware.title")}>
          <VmwareFields value={vcenter} onChange={setVcenter} />
          <VmwareTest
            siteId={siteId}
            address={address}
            probe={probe}
            value={vcenter}
            onTrust={(fingerprint) => setVcenter({ ...vcenter, fingerprint: fingerprint ?? undefined })}
            onTested={setVcenterTested}
          />
          {vcenterError && (
            <p className={styles.formError} role="alert">
              {vcenterError}
            </p>
          )}
        </fieldset>
      )}
      <div className={styles.formFoot}>
        <Button variant="k" type="submit" disabled={busy || !siteId}>
          {busy ? t("devices.add.adding") : t("devices.add.button")}
        </Button>
        <Button onClick={() => router.push(scopedHref("/", scope))} disabled={busy}>
          {t("devices.add.cancel")}
        </Button>
      </div>
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
    </form>
  );
}

/** What a detection found, in words. */
function DetectedLine({ detected }: { detected: Detected }) {
  if (!detected.answers) {
    return (
      <p className={styles.formError} role="status">
        {t("devices.snmp.noAnswer", {
          probe: detected.probe === "central" ? t("devices.from.short.central") : detected.probe,
          error: detected.error ?? "",
        })}
      </p>
    );
  }
  const description = detected.description ?? "";
  const groups = detected.groups.map((g) => t(`devices.snmp.group.${g as "interfaces"}`)).join(", ");
  return (
    <p className={styles.sectionNote} role="status">
      {detected.name ? t("devices.snmp.answersNamed", { name: detected.name, description }) : t("devices.snmp.answers", { description })}
      {groups && (
        <>
          <br />
          {t("devices.snmp.groups", { groups })}
        </>
      )}
    </p>
  );
}
