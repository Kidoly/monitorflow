import { afterEach, describe, expect, it } from "vitest";

import { setActiveLocale } from "@/i18n/locale";

import type { MaintenanceWindowRequest } from "./api";
import { checkName, coversCheck, describeNext, describeOccurrence, describeRule, describeWhat } from "./maintenance";

const weekly: MaintenanceWindowRequest = {
  site_id: "s",
  server_ids: [],
  repeat: "weekly",
  every: 1,
  weekdays: ["sunday"],
  months: [],
  start_time: "02:00",
  duration_minutes: 120,
  time_zone: "Europe/Paris",
  reason: "Patch night",
};

describe("maintenance windows in words", () => {
  afterEach(() => setActiveLocale("en"));

  it("say how a rule comes back", () => {
    expect(describeRule(weekly)).toBe("Every Sunday, 02:00 to 04:00 (Europe/Paris)");
    expect(describeRule({ ...weekly, weekdays: ["monday", "sunday"], every: 2 })).toBe(
      "Every 2 weeks on Monday and Sunday, 02:00 to 04:00 (Europe/Paris)",
    );
    expect(describeRule({ ...weekly, weekdays: ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"] })).toBe(
      "Every day, 02:00 to 04:00 (Europe/Paris)",
    );
    expect(describeRule({ ...weekly, repeat: "daily", every: 3 })).toBe("Every 3 days, 02:00 to 04:00 (Europe/Paris)");
    expect(
      describeRule({ ...weekly, repeat: "monthly", weekdays: ["tuesday"], month_week: 2, start_time: "22:00", duration_minutes: 240 }),
    ).toBe("On the second Tuesday of every month, 22:00 to 02:00 the next day (Europe/Paris)");
    expect(describeRule({ ...weekly, repeat: "monthly", weekdays: ["friday"], month_week: 5, months: [7, 1] })).toBe(
      "On the last Friday of January and July, 02:00 to 04:00 (Europe/Paris)",
    );
    expect(describeRule({ ...weekly, repeat: "monthly", weekdays: [], month_day: 1, time_zone: "America/New_York" })).toBe(
      "On day 1 of every month, 02:00 to 04:00 (America/New York)",
    );
    expect(describeRule({ ...weekly, weekdays: ["saturday"], start_time: "00:00", duration_minutes: 2 * 24 * 60 + 360 })).toBe(
      "Every Saturday, 00:00 to 06:00 2 days later (Europe/Paris)",
    );
    expect(describeRule({ ...weekly, start_time: "00:00", duration_minutes: 1440 })).toBe("Every Sunday, all day (Europe/Paris)");
  });

  it("say it in French", () => {
    setActiveLocale("fr");
    expect(describeRule(weekly)).toBe("Chaque dimanche, de 02:00 à 04:00 (Europe/Paris)");
    expect(describeRule({ ...weekly, repeat: "monthly", weekdays: [], month_day: 1 })).toBe(
      "Le 1er de chaque mois, de 02:00 à 04:00 (Europe/Paris)",
    );
    expect(
      describeRule({ ...weekly, repeat: "monthly", weekdays: ["tuesday"], month_week: 2, start_time: "22:00", duration_minutes: 240 }),
    ).toBe("Le deuxième mardi de chaque mois, de 22:00 à 02:00 le lendemain (Europe/Paris)");
  });

  it("show the next windows in the window's own time", () => {
    // Clocks go forward in Paris on 2026-03-29.
    const night = { starts_at: "2026-03-28T21:00:00Z", ends_at: "2026-03-29T04:00:00Z" };
    expect(describeOccurrence(night, "Europe/Paris")).toBe("Saturday 28 March, 22:00 to Sunday 29 March, 06:00");
    const sunday = { starts_at: "2026-11-22T01:00:00Z", ends_at: "2026-11-22T03:00:00Z" };
    expect(describeOccurrence(sunday, "Europe/Paris")).toBe("Sunday 22 November, 02:00 to 04:00");
    // The running one is not the next.
    const running = { starts_at: "2026-11-15T01:00:00Z", ends_at: "2026-11-15T03:00:00Z" };
    expect(describeNext({ next: [running, sunday], time_zone: "Europe/Paris" }, new Date("2026-11-15T02:00:00Z"))).toBe(
      "Next: Sunday 22 November, 02:00 to 04:00",
    );
    expect(describeNext({ next: [], time_zone: "UTC" }, new Date())).toBe("No window left: its last day is past.");
  });

  it("say what a window covers", () => {
    expect(describeWhat({ site_name: "paris", servers: [], checks: null })).toBe("All of site paris");
    const servers = ["web-1", "web-2", "db-1", "db-2"].map((hostname) => ({ id: hostname, hostname }));
    expect(describeWhat({ site_name: "paris", servers, checks: null })).toBe("web-1, web-2, db-1 and 1 more");
    expect(describeWhat({ site_name: "paris", servers: servers.slice(0, 1), checks: [{ name: "disk", instance: "/var" }] })).toBe(
      "web-1: disk /var",
    );
    expect(describeWhat({ site_name: "paris", servers: [], checks: null, whole_site: false })).toBe(
      "No server left: its servers were removed",
    );
    expect(checkName({ name: "service_nginx", instance: "" })).toBe("service_nginx");
    expect(coversCheck([{ name: "disk", instance: "" }], "disk", "/var")).toBe(true);
    expect(coversCheck([{ name: "disk", instance: "/" }], "disk", "/var")).toBe(false);
  });
});
