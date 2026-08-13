import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fallbackBootstrap } from "../data";
import type { ModelInstallPlan } from "../types";
import { ModelsView } from "./ModelsView";

const bridge = vi.hoisted(() => ({
  cancelModelInstall: vi.fn(),
  cancelJob: vi.fn(),
  getEngineHealth: vi.fn(),
  getModelInstallPlan: vi.fn(),
  installModel: vi.fn(),
  listJobs: vi.fn(),
  queueModelRuntimeLoad: vi.fn(),
  removeModel: vi.fn(),
  setupEngineRuntime: vi.fn(),
  unloadModelRuntime: vi.fn(),
  verifyModel: vi.fn(),
}));

vi.mock("../lib/bridge", () => bridge);

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

const revision = "a".repeat(40);
const damagedBootstrap = {
  ...fallbackBootstrap,
  installed: fallbackBootstrap.installed.map((model) => model.model_id === "hexgrad/Kokoro-82M"
    ? {
        ...model,
        revision,
        integrity: {
          state: "repair-needed" as const,
          reason: "incomplete-files" as const,
          missing_files: ["kokoro-v1_0.pth"],
          invalid_files: [],
          checked_files: 2,
          installed_size_bytes: 2,
          manifest_verified: true,
        },
      }
    : model),
};

const plan: ModelInstallPlan = {
  model_id: "hexgrad/Kokoro-82M",
  source_url: "https://huggingface.co/hexgrad/Kokoro-82M",
  revision,
  license: "apache-2.0",
  access: "public",
  download_size_bytes: 9,
  file_count: 2,
  recommended_for_12gb: true,
  model_cache_dir: "/models/hexgrad__Kokoro-82M",
};

describe("Models registry", () => {
  it("keeps damaged installs visible and offers pinned repair approval", async () => {
    const user = userEvent.setup();
    bridge.getModelInstallPlan.mockResolvedValue(plan);

    render(<ModelsView bootstrap={damagedBootstrap} onChanged={vi.fn().mockResolvedValue(undefined)} />);

    expect(screen.getAllByText("Repair needed").length).toBeGreaterThan(0);
    await user.click(screen.getByRole("button", { name: "More actions for Kokoro-82M" }));
    await user.click(screen.getByRole("menuitem", { name: "Repair model" }));
    expect(await screen.findByRole("heading", { name: "Repair Kokoro-82M" })).toBeVisible();
    expect(bridge.getModelInstallPlan).toHaveBeenCalledWith("hexgrad/Kokoro-82M", revision);
    const repair = screen.getByRole("button", { name: "Download and repair" });
    expect(repair).toBeDisabled();
    await user.click(screen.getByRole("checkbox"));
    expect(repair).toBeEnabled();
  });

  it("scopes health results to the selected model", async () => {
    const user = userEvent.setup();
    bridge.getEngineHealth.mockResolvedValue({
      status: "ready",
      device: "cuda",
      engine_scope: "kokoro",
      engine_runtime: "pinned",
      process_id: 1,
      warm_workers: 1,
      worker_starts: 1,
      worker_restarts: 0,
      worker_failures: 0,
      loaded_models: ["hexgrad/Kokoro-82M"],
    });
    bridge.verifyModel.mockResolvedValue(
      damagedBootstrap.installed.find((model) => model.model_id === "hexgrad/Kokoro-82M")!.integrity,
    );

    render(<ModelsView bootstrap={damagedBootstrap} onChanged={vi.fn().mockResolvedValue(undefined)} />);
    await user.click(screen.getByText("Kokoro-82M", { selector: "strong" }));
    await user.click(screen.getByRole("button", { name: "Health" }));
    expect(await screen.findByText(/repair needed.*ready on cuda/i)).toBeVisible();
    expect(screen.getByText(/Kokoro-82M loaded/i)).toBeVisible();

    await user.click(screen.getByText("chatterbox", { selector: "strong" }));
    await waitFor(() => expect(screen.queryByText(/repair needed.*ready on cuda/i)).not.toBeInTheDocument());
  });

  it("queues and cancels model prewarming through the durable task system", async () => {
    const user = userEvent.setup();
    const nativeBootstrap = { ...fallbackBootstrap, runtime: "tauri" as const };
    const loadJob = { id: "load-1", kind: "model-load", status: "preparing" as const, progress: 0.05, attempt: 1, model_id: "hexgrad/Kokoro-82M", created_at: "now", updated_at: "now" };
    bridge.queueModelRuntimeLoad.mockResolvedValue(loadJob);
    bridge.listJobs.mockResolvedValue([loadJob]);
    bridge.cancelJob.mockResolvedValue(true);

    render(<ModelsView bootstrap={nativeBootstrap} onChanged={vi.fn().mockResolvedValue(undefined)} />);
    await user.click(screen.getByText("Kokoro-82M", { selector: "strong" }));
    await user.click(screen.getByRole("button", { name: "Load model" }));
    expect(bridge.queueModelRuntimeLoad).toHaveBeenCalledWith("hexgrad/Kokoro-82M");
    await user.click(await screen.findByRole("button", { name: "Cancel load" }));
    expect(bridge.cancelJob).toHaveBeenCalledWith("load-1");
    expect(await screen.findByText(/cancelling model load/i)).toBeVisible();
    expect(screen.getByRole("button", { name: "Cancelling" })).toBeDisabled();
  });

  it("starts with bootstrap-reported residency and can unload it", async () => {
    const user = userEvent.setup();
    const nativeBootstrap = {
      ...fallbackBootstrap,
      runtime: "tauri" as const,
      engine_runtimes: fallbackBootstrap.engine_runtimes.map((runtime) => runtime.engine === "kokoro" ? { ...runtime, warm_workers: 1, loaded_models: ["hexgrad/Kokoro-82M"] } : runtime),
    };
    bridge.unloadModelRuntime.mockResolvedValue({ status: "unloaded", engine: "kokoro", model_id: "hexgrad/Kokoro-82M", retired_workers: 1, unloaded_models: ["hexgrad/Kokoro-82M"] });
    render(<ModelsView bootstrap={nativeBootstrap} onChanged={vi.fn().mockResolvedValue(undefined)} />);
    await user.click(screen.getByText("Kokoro-82M", { selector: "strong" }));
    await user.click(screen.getByRole("button", { name: "Unload model" }));
    expect(bridge.unloadModelRuntime).toHaveBeenCalledWith("hexgrad/Kokoro-82M");
  });
});
