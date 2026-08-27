import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AppShell } from "./AppShell";
import { PageHeader } from "./ui";
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

afterEach(cleanup);

describe("AppShell capability navigation", () => {
  it("keeps page-specific headings and actions in the page while the desktop strip stays structural", () => {
    render(
      <AppShell current="generate" onNavigate={vi.fn()} theme="dark" onToggleTheme={vi.fn()} system={system} runtime="tauri" features={features}>
        <section className="page">
          <PageHeader title="Generate" subtitle="Create speech locally." actions={<button type="button">Queue audio</button>} />
        </section>
      </AppShell>,
    );

    const heading = screen.getByRole("heading", { name: "Generate" });
    expect(heading.closest(".app-content")).not.toBeNull();
    expect(document.querySelector(".topbar-page-icon")).toBeNull();
    expect(document.querySelector(".page-toolbar-slot")).toBeNull();
    expect(document.querySelector(".topbar-drag-region")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Queue audio" }).closest(".page-header-content")).not.toBeNull();
    expect(screen.getByRole("button", { name: "Minimize window" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Maximize window" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Close window" })).toBeInTheDocument();
    expect(document.querySelectorAll(".window-resize-handle")).toHaveLength(8);
    expect(document.querySelector(".app-topbar")).toHaveAttribute("data-tauri-drag-region");
    expect(document.querySelector(".app-content .page-header")).not.toBeNull();
  });

  it("removes capture routes and keeps maturity details in native tooltips", () => {
    const navigate = vi.fn();
    render(
      <AppShell current="generate" onNavigate={navigate} theme="dark" onToggleTheme={vi.fn()} system={system} runtime="tauri" features={features}>
        <div>Content</div>
      </AppShell>,
    );

    expect(screen.queryByRole("button", { name: "Live" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Transcribe" })).not.toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "Compare" })[0]).toHaveAttribute("title", "Compare (experimental)");
    expect(screen.getAllByRole("button", { name: "Voices" }).some((button) => button.title === "Voices (beta)")).toBe(true);
  });

  it("provides desktop menus, navigation history, and a collapsible rail", () => {
    const navigate = vi.fn();
    render(
      <AppShell current="generate" onNavigate={navigate} theme="light" onToggleTheme={vi.fn()} system={system} runtime="tauri" features={features}>
        <div>Content</div>
      </AppShell>,
    );

    fireEvent.click(screen.getByRole("button", { name: "File" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Projects" }));
    expect(navigate).toHaveBeenLastCalledWith("projects");
    expect(screen.getByRole("button", { name: "Go back" })).not.toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "Hide sidebar" }));
    expect(document.querySelector(".app-shell")).toHaveClass("is-sidebar-collapsed");
    expect(screen.getByRole("button", { name: "Show sidebar" })).toBeInTheDocument();
  });

  it("keeps the sidebar open until the user explicitly collapses it", () => {
    const originalMatchMedia = window.matchMedia;
    window.matchMedia = vi.fn().mockReturnValue({
      matches: true,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    });

    render(
      <AppShell current="projects" onNavigate={vi.fn()} theme="light" onToggleTheme={vi.fn()} system={system} runtime="browser" features={features}>
        <div>Content</div>
      </AppShell>,
    );

    expect(document.querySelector(".app-shell")).not.toHaveClass("is-sidebar-collapsed");
    expect(screen.getByRole("button", { name: "Hide sidebar" })).toBeInTheDocument();
    expect(document.querySelector(".window-resize-handles")).toBeNull();
    window.matchMedia = originalMatchMedia;
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
    fireEvent.click(within(menu!).getByRole("button", { name: "Compare" }));
    expect(navigate).toHaveBeenCalledWith("compare");
    expect(document.getElementById("mobile-more-menu")).toBeNull();
  });
});
