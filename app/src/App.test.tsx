import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fallbackBootstrap } from "./data";

const bridge = vi.hoisted(() => ({
  loadBootstrapState: vi.fn(),
  listHistory: vi.fn(),
  listJobs: vi.fn(),
  listProjects: vi.fn(),
  loadGeneratedAudio: vi.fn(),
  loadJobPreview: vi.fn(),
  exportHistoryItem: vi.fn(),
  saveApplicationSetting: vi.fn(),
}));

vi.mock("./lib/bridge", () => bridge);
vi.mock("@tauri-apps/plugin-updater", () => ({ check: vi.fn() }));
// The chat canvas is the launch surface, so App now mounts the assistant on every successful
// bootstrap. Stubbing Codex keeps this test on the runtime boundary it is about.
vi.mock("./lib/codexBridge", () => ({
  refreshCodexConnection: vi.fn().mockResolvedValue({ available: false, connected: false, message: "Codex is not installed." }),
  codexRequest: vi.fn(),
  listenToCodex: vi.fn().mockResolvedValue(() => undefined),
  loadCodexModels: vi.fn().mockResolvedValue([]),
  loadAssistantVideoThreadLink: vi.fn().mockResolvedValue(undefined),
  respondToCodex: vi.fn(),
}));

import App from "./App";

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("App native bootstrap boundary", () => {
  it("fails closed without preview data and hydrates only after a successful retry", async () => {
    const user = userEvent.setup();
    const nativeState = {
      ...fallbackBootstrap,
      runtime: "tauri" as const,
      voices: [],
      installed: [],
      catalog: [],
      projects: [],
      transcriptions: [],
    };
    bridge.loadBootstrapState
      .mockRejectedValueOnce(new Error("Database integrity check failed"))
      .mockResolvedValueOnce(nativeState);
    bridge.listHistory.mockResolvedValue([]);
    bridge.listJobs.mockResolvedValue([]);
    bridge.listProjects.mockResolvedValue([]);
    bridge.saveApplicationSetting.mockResolvedValue(nativeState.settings);

    render(<App />);

    expect(await screen.findByRole("heading", { name: "Local runtime unavailable" })).toBeVisible();
    expect(screen.getByRole("alert")).toHaveTextContent("Database integrity check failed");
    expect(screen.queryByRole("heading", { name: "New generation" })).not.toBeInTheDocument();
    expect(screen.queryByText("Mara")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(bridge.loadBootstrapState).toHaveBeenCalledTimes(2));
    // A hydrated app lands on the chat canvas; the classic studio is one toggle away.
    expect(await screen.findByRole("heading", { name: "Hello" })).toBeVisible();
    expect(screen.queryByRole("heading", { name: "New generation" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Classic" }));
    expect(await screen.findByRole("heading", { name: "New generation" })).toBeVisible();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("does not expose partially hydrated native state when history loading fails", async () => {
    bridge.loadBootstrapState.mockResolvedValue({ ...fallbackBootstrap, runtime: "tauri" as const });
    bridge.listHistory.mockRejectedValue(new Error("History store could not be read"));

    render(<App />);

    expect(await screen.findByRole("heading", { name: "Local runtime unavailable" })).toBeVisible();
    expect(screen.getByRole("alert")).toHaveTextContent("History store could not be read");
    expect(screen.queryByRole("heading", { name: "New generation" })).not.toBeInTheDocument();
  });
});
