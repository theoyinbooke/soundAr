import { fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { AppShell } from "./AppShell";
import type { FeatureState } from "../types";

const system = {
  gpu_name: "Test GPU",
  vram_total_mb: 12_288,
  vram_used_mb: 1_024,
  driver_version: "test",
  cuda_available: true,
  python_ready: true,
};

const features: Record<string, FeatureState> = {
  generate: "stable",
  voices: "beta",
  models: "beta",
  live: "disabled",
  compare: "experimental",
  benchmarks: "experimental",
  history: "beta",
};

describe("AppShell capability navigation", () => {
  it("disables unavailable routes and labels non-stable routes", () => {
    const navigate = vi.fn();
    render(
      <AppShell current="generate" onNavigate={navigate} theme="dark" onToggleTheme={vi.fn()} system={system} runtime="tauri" features={features}>
        <div>Content</div>
      </AppShell>,
    );

    const liveButtons = screen.getAllByRole("button", { name: /live/i });
    expect(liveButtons.every((button) => button.hasAttribute("disabled"))).toBe(true);
    fireEvent.click(liveButtons[0]);
    expect(navigate).not.toHaveBeenCalled();
    expect(screen.getAllByText("Labs").length).toBeGreaterThan(0);
    expect(screen.getAllByText("beta").length).toBeGreaterThan(0);
  });

  it("keeps secondary destinations available through compact navigation", () => {
    const navigate = vi.fn();
    render(
      <AppShell current="generate" onNavigate={navigate} theme="dark" onToggleTheme={vi.fn()} system={system} runtime="tauri" features={features}>
        <div>Content</div>
      </AppShell>,
    );

    fireEvent.click(screen.getAllByRole("button", { name: "More navigation" }).at(-1)!);
    const menu = document.getElementById("mobile-more-menu");
    expect(menu).not.toBeNull();
    fireEvent.click(within(menu!).getByRole("button", { name: "History" }));
    expect(navigate).toHaveBeenCalledWith("history");
    expect(document.getElementById("mobile-more-menu")).toBeNull();
  });
});
