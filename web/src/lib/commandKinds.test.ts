import { describe, expect, it } from "vitest";

import { acceptance, aiCommands, aiSwitch, commandSummary, unsupportedReason } from "./commandKinds";

describe("aiSwitch", () => {
  it("turns on and off where the host allowed it, only off where not, nothing for an old agent", () => {
    expect(aiSwitch({ command_kinds: { ai_commands: "accepted" } })).toBe("both");
    expect(aiSwitch({ command_kinds: { ai_commands: "off" } })).toBe("off");
    expect(aiSwitch({ command_kinds: { shell: "accepted" } })).toBe("none");
    expect(aiSwitch({ command_kinds: null })).toBe("none");
  });
});

describe("acceptance", () => {
  it("keeps the API's words and calls the rest unknown", () => {
    expect(acceptance("accepted")).toBe("accepted");
    expect(acceptance("off")).toBe("off");
    expect(acceptance("unsupported")).toBe("unsupported");
    expect(acceptance("from a newer server")).toBe("unknown");
    expect(acceptance(null)).toBe("unknown");
    expect(acceptance(undefined)).toBe("unknown");
  });
});

describe("aiCommands", () => {
  it("is unknown for an agent that does not report, never a yes or a no", () => {
    expect(aiCommands({})).toBe("unknown");
    expect(aiCommands({ command_kinds: null })).toBe("unknown");
    expect(aiCommands({ command_kinds: { apply_updates: "accepted" } })).toBe("unknown");
    expect(aiCommands({ command_kinds: { shell: "off" } })).toBe("off");
  });
});

describe("commandSummary", () => {
  it("says nothing while the agent has not reported", () => {
    expect(commandSummary({ command_kinds: null })).toBeNull();
  });

  it("names what is accepted and what is refused, and leaves out the unknown", () => {
    expect(
      commandSummary({
        command_kinds: { apply_updates: "accepted", configure_sources: "unsupported", self_update: "accepted", shell: "off" },
      }),
    ).toBe("Accepts package updates, agent updates. Refuses package sources, AI commands.");
    expect(commandSummary({ command_kinds: { self_update: "accepted" } })).toBe("Accepts agent updates.");
  });
});

describe("unsupportedReason", () => {
  it("sends Windows to an agent update", () => {
    expect(unsupportedReason("windows")).toBe("Its agent is too old for AI commands on Windows. It updates by itself.");
    expect(unsupportedReason("linux")).toBe("This system does not run AI commands yet.");
  });
});
