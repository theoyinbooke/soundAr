import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: tauri.listen }));

import { refreshCodexConnection } from "./codexBridge";

describe("native Codex bridge discovery", () => {
  beforeEach(() => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      configurable: true,
      value: {},
    });
    tauri.invoke.mockReset();
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    delete (window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  it("keeps retrying a cold unavailable scan and deduplicates concurrent refreshes", async () => {
    tauri.invoke
      .mockResolvedValueOnce({ available: false, connected: false, message: "Codex CLI was not found." })
      .mockResolvedValueOnce({ available: false, connected: false, message: "Codex CLI was not found." })
      .mockResolvedValueOnce({ available: false, connected: false, message: "Codex CLI was not found." })
      .mockResolvedValueOnce({ available: true, connected: false, path: "/opt/codex/bin/codex", version: "codex-cli 0.151.0" })
      .mockResolvedValueOnce({ available: true, connected: true, path: "/opt/codex/bin/codex", version: "codex-cli 0.151.0" });

    const first = refreshCodexConnection();
    const concurrent = refreshCodexConnection();

    expect(concurrent).toBe(first);
    await vi.runAllTimersAsync();
    await expect(first).resolves.toMatchObject({ available: true, connected: true });
    expect(tauri.invoke.mock.calls.map(([command]) => command)).toEqual([
      "codex_agent_status",
      "codex_agent_status",
      "codex_agent_status",
      "codex_agent_status",
      "codex_agent_connect",
    ]);
  });

  it("makes one final native connection attempt before reporting a cold install as unavailable", async () => {
    const unavailable = { available: false, connected: false, message: "Codex CLI was not found." };
    for (let attempt = 0; attempt < 7; attempt += 1) tauri.invoke.mockResolvedValueOnce(unavailable);
    tauri.invoke.mockResolvedValueOnce({ connected: true, path: "/opt/codex/bin/codex", version: "codex-cli 0.151.0" });

    const refresh = refreshCodexConnection();
    await vi.runAllTimersAsync();

    await expect(refresh).resolves.toMatchObject({ available: true, connected: true });
    expect(tauri.invoke.mock.calls.map(([command]) => command)).toEqual([
      "codex_agent_status",
      "codex_agent_status",
      "codex_agent_status",
      "codex_agent_status",
      "codex_agent_status",
      "codex_agent_status",
      "codex_agent_status",
      "codex_agent_connect",
    ]);
  });
});
