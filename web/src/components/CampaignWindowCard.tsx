"use client";

import { t } from "@/i18n";
import {
  MONTHS,
  MONTH_WEEKS,
  WEEKDAYS,
  type WindowDraft,
  changeKind,
  describeWindow,
  monthName,
  toRequest,
  emptyDraft,
  weekdayLong,
  weekdayName,
} from "@/lib/campaigns";

import own from "./Campaigns.module.css";
import { Button, fieldClass } from "./ui/Button";

const KINDS: WindowDraft["kind"][] = ["weekly", "monthly", "yearly", "once"];

/**
 * One maintenance window as a card: how it repeats, its days as toggles
 * (weekdays, or a day of the month, or months), its hours and time zone
 * on one line, and the rule in words.
 */
export function CampaignWindowCard({
  index,
  window,
  onChange,
  onRemove,
}: {
  index: number;
  window: WindowDraft;
  onChange: (window: WindowDraft) => void;
  onRemove: () => void;
}) {
  const number = index + 1;
  const label = t("campaigns.check.window", { number });
  const set = (patch: Partial<WindowDraft>) => onChange({ ...window, ...patch });
  const monthly = window.kind === "monthly" || window.kind === "yearly";
  const request = toRequest({ ...emptyDraft(), windows: [window] }).windows[0]!;
  const day = Number(window.monthDay);

  return (
    <div className={own.windowCard} role="group" aria-label={label}>
      <div className={own.cardHead}>
        <strong>{label}</strong>
        <label className={own.inline}>
          {t("campaigns.form.repeat")}
          <select
            className={fieldClass}
            value={window.kind}
            onChange={(e) => onChange(changeKind(window, e.target.value as WindowDraft["kind"]))}
          >
            {KINDS.map((kind) => (
              <option key={kind} value={kind}>
                {t(`campaigns.form.kind.${kind}`)}
              </option>
            ))}
          </select>
        </label>
        <Button size="small" onClick={onRemove} aria-label={t("campaigns.form.removeWindow", { number })}>
          {t("campaigns.form.remove")}
        </Button>
      </div>

      {window.kind === "weekly" && (
        <Pills
          label={t("campaigns.form.days")}
          values={WEEKDAYS}
          chosen={window.weekdays}
          name={weekdayName}
          title={weekdayLong}
          onChange={(weekdays) => set({ weekdays })}
        />
      )}

      {monthly && (
        <div className={own.row}>
          {window.kind === "yearly" && (
            <label className={own.fld}>
              {t("campaigns.form.month")}
              <select
                className={fieldClass}
                value={window.months[0] ?? 1}
                onChange={(e) => set({ months: [Number(e.target.value)] })}
              >
                {MONTHS.map((m) => (
                  <option key={m} value={m}>
                    {monthName(m)}
                  </option>
                ))}
              </select>
            </label>
          )}
          <label className={own.fld}>
            {t("campaigns.form.monthMode")}
            <select
              className={fieldClass}
              value={window.monthMode}
              onChange={(e) => set({ monthMode: e.target.value as WindowDraft["monthMode"] })}
            >
              <option value="day">{t("campaigns.form.onDay")}</option>
              <option value="weekday">{t("campaigns.form.onWeekday")}</option>
            </select>
          </label>
          {window.monthMode === "day" ? (
            <label className={own.fld}>
              {t("campaigns.form.monthDay")}
              <input
                className={`${fieldClass} ${own.short}`}
                type="number"
                min={1}
                max={31}
                value={window.monthDay}
                onChange={(e) => set({ monthDay: e.target.value })}
              />
            </label>
          ) : (
            <>
              <label className={own.fld}>
                {t("campaigns.form.week")}
                <select
                  className={fieldClass}
                  value={window.monthWeek}
                  onChange={(e) => set({ monthWeek: Number(e.target.value) })}
                >
                  {MONTH_WEEKS.map((w) => (
                    <option key={w} value={w}>
                      {t(`campaigns.window.week${w}`)}
                    </option>
                  ))}
                </select>
              </label>
              <label className={own.fld}>
                {t("campaigns.form.weekday")}
                <select
                  className={fieldClass}
                  value={window.weekday}
                  onChange={(e) => set({ weekday: Number(e.target.value) })}
                >
                  {WEEKDAYS.map((d) => (
                    <option key={d} value={d}>
                      {weekdayLong(d)}
                    </option>
                  ))}
                </select>
              </label>
            </>
          )}
        </div>
      )}
      {window.kind === "monthly" && (
        <Pills
          label={t("campaigns.form.months")}
          values={MONTHS}
          chosen={window.months}
          name={(m) => monthName(m, true)}
          title={(m) => monthName(m)}
          onChange={(months) => set({ months })}
        />
      )}
      {monthly && window.monthMode === "day" && day > 28 && (
        <p className={own.note}>{t("campaigns.form.skippedMonths", { day })}</p>
      )}

      {window.kind === "once" ? (
        <div className={own.row}>
          <label className={own.fld}>
            {t("campaigns.form.starts")}
            <input
              className={fieldClass}
              type="datetime-local"
              value={window.startsAt}
              onChange={(e) => set({ startsAt: e.target.value })}
            />
          </label>
          <label className={own.fld}>
            {t("campaigns.form.ends")}
            <input
              className={fieldClass}
              type="datetime-local"
              value={window.endsAt}
              onChange={(e) => set({ endsAt: e.target.value })}
            />
          </label>
        </div>
      ) : (
        <>
          <div className={own.row}>
            <label className={own.fld}>
              {t("campaigns.form.from")}
              <input className={fieldClass} type="time" value={window.start} onChange={(e) => set({ start: e.target.value })} />
            </label>
            <label className={own.fld}>
              {t("campaigns.form.to")}
              <input className={fieldClass} type="time" value={window.end} onChange={(e) => set({ end: e.target.value })} />
            </label>
            <label className={own.fld}>
              {t("campaigns.form.timezone")}
              <input className={fieldClass} value={window.timezone} onChange={(e) => set({ timezone: e.target.value })} />
            </label>
          </div>
          <p className={own.note}>{describeWindow(request)}</p>
        </>
      )}
    </div>
  );
}

/** Toggle buttons for a set of numbers: weekdays or months. */
function Pills({
  label,
  values,
  chosen,
  name,
  title,
  onChange,
}: {
  label: string;
  values: readonly number[];
  chosen: number[];
  name: (value: number) => string;
  title: (value: number) => string;
  onChange: (chosen: number[]) => void;
}) {
  return (
    <div className={own.pills} role="group" aria-label={label}>
      {values.map((value) => {
        const on = chosen.includes(value);
        return (
          <button
            key={value}
            type="button"
            className={own.pill}
            aria-pressed={on}
            title={title(value)}
            onClick={() => onChange(on ? chosen.filter((v) => v !== value) : values.filter((v) => v === value || chosen.includes(v)))}
          >
            {name(value)}
          </button>
        );
      })}
    </div>
  );
}
