import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AppShell } from "./AppShell";
import { PageHeader } from "./ui";
import type { FeatureState } from "../types";
import { VideoIntegrationProvider } from "./video/VideoIntegrationContext";
import { createBrowserPreviewVideoService } from "../lib/videoBridge";

// Chat mode mounts the assistant, so keep it offline and off the studio poller: these tests are
// about how the shell places the pane, not about what Codex says back.
vi.mock("../lib/codexBridge", () => ({
  refreshCodexConnection: vi.fn().mockResolvedValue({ available: false, connected: false, message: "Codex is not installed." }),
  codexRequest: vi.fn(),
  listenToCodex: vi.fn().mockResolvedValue(() => undefined),
  loadCodexModels: vi.fn().mockResolvedValue([]),
  loadAssistantVideoThreadLink: vi.fn().mockResolvedValue(undefined),
  respondToCodex: vi.fn(),
}));
vi.mock("../lib/bridge", () => ({
  listHistory: vi.fn().mockResolvedValue([]),
  listJobs: vi.fn().mockResolvedValue([]),
  loadGeneratedAudio: vi.fn(),
  loadJobPreview: vi.fn(),
  exportHistoryItem: vi.fn(),
}));

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
      <AppShell current="generate" onNavigate={vi.fn()} theme="dark" onToggleTheme={vi.fn()} system={system} runtime="tauri" features={features} assistantOpen={false} onAssistantOpenChange={vi.fn()} chatMode={false} onChatModeChange={vi.fn()}>
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
      <AppShell current="generate" onNavigate={navigate} theme="dark" onToggleTheme={vi.fn()} system={system} runtime="tauri" features={features} assistantOpen={false} onAssistantOpenChange={vi.fn()} chatMode={false} onChatModeChange={vi.fn()}>
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
      <AppShell current="generate" onNavigate={navigate} theme="light" onToggleTheme={vi.fn()} system={system} runtime="tauri" features={features} assistantOpen={false} onAssistantOpenChange={vi.fn()} chatMode={false} onChatModeChange={vi.fn()}>
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
      <AppShell current="projects" onNavigate={vi.fn()} theme="light" onToggleTheme={vi.fn()} system={system} runtime="browser" features={features} assistantOpen={false} onAssistantOpenChange={vi.fn()} chatMode={false} onChatModeChange={vi.fn()}>
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
      <AppShell current="generate" onNavigate={navigate} theme="dark" onToggleTheme={vi.fn()} system={system} runtime="tauri" features={features} assistantOpen={false} onAssistantOpenChange={vi.fn()} chatMode={false} onChatModeChange={vi.fn()}>
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

  it("merges video projects into Recent and previews the exact project rather than opening the editor", async () => {
    const openProject = vi.fn();
    const previewProject = vi.fn();
    render(
      <VideoIntegrationProvider service={createBrowserPreviewVideoService()} onOpenProject={openProject} onPreviewProject={previewProject}>
        <AppShell current="generate" onNavigate={vi.fn()} theme="light" onToggleTheme={vi.fn()} system={system} runtime="browser" features={features} history={[]} assistantOpen={false} onAssistantOpenChange={vi.fn()} chatMode={false} onChatModeChange={vi.fn()}>
          <div>Content</div>
        </AppShell>
      </VideoIntegrationProvider>,
    );

    await waitFor(() => expect(screen.getByRole("navigation", { name: "Recent work" })).toBeInTheDocument());
    const video = screen.getByTitle("Creator update · Reel master");
    expect(within(video).getByText("Video")).toBeInTheDocument();
    fireEvent.click(video);
    // Finished work plays where it is selected; entering the editor stays a deliberate action.
    expect(previewProject).toHaveBeenCalledWith("creator-update-master");
    expect(openProject).not.toHaveBeenCalled();
  });

  it("marks the selected video as current while its master is showing in History", async () => {
    render(
      <VideoIntegrationProvider service={createBrowserPreviewVideoService()} onOpenProject={vi.fn()} onPreviewProject={vi.fn()} activeProjectId="creator-update-master">
        <AppShell current="history" onNavigate={vi.fn()} theme="light" onToggleTheme={vi.fn()} system={system} runtime="browser" features={features} history={[]} assistantOpen={false} onAssistantOpenChange={vi.fn()} chatMode={false} onChatModeChange={vi.fn()}>
          <div>Content</div>
        </AppShell>
      </VideoIntegrationProvider>,
    );

    await waitFor(() => expect(screen.getByRole("navigation", { name: "Recent work" })).toBeInTheDocument());
    expect(screen.getByTitle("Creator update · Reel master")).toHaveAttribute("aria-current", "page");
  });

  it("still opens the editor when no preview handler is provided", async () => {
    const openProject = vi.fn();
    render(
      <VideoIntegrationProvider service={createBrowserPreviewVideoService()} onOpenProject={openProject}>
        <AppShell current="generate" onNavigate={vi.fn()} theme="light" onToggleTheme={vi.fn()} system={system} runtime="browser" features={features} history={[]} assistantOpen={false} onAssistantOpenChange={vi.fn()} chatMode={false} onChatModeChange={vi.fn()}>
          <div>Content</div>
        </AppShell>
      </VideoIntegrationProvider>,
    );

    await waitFor(() => expect(screen.getByRole("navigation", { name: "Recent work" })).toBeInTheDocument());
    fireEvent.click(screen.getByTitle("Creator update · Reel master"));
    expect(openProject).toHaveBeenCalledWith("creator-update-master");
  });

  it("hands the whole content cell to the chat canvas and keeps the classic views out of it", async () => {
    render(
      <AppShell current="generate" onNavigate={vi.fn()} theme="light" onToggleTheme={vi.fn()} system={system} runtime="tauri" features={features} assistantOpen={false} onAssistantOpenChange={vi.fn()} chatMode onChatModeChange={vi.fn()}>
        <section className="page">
          <PageHeader title="New generation" subtitle="Create speech locally." />
        </section>
      </AppShell>,
    );

    expect(await screen.findByRole("heading", { name: "Hello" })).toBeVisible();
    expect(screen.getByLabelText("Message soundAr assistant")).toBeInTheDocument();
    // The classic view is not merely hidden — it never renders, so its effects stay idle.
    expect(screen.queryByRole("heading", { name: "New generation" })).not.toBeInTheDocument();
    expect(document.querySelector(".app-shell")).toHaveClass("is-chat-mode");
    expect(document.querySelector(".assistant-canvas")).toBeInTheDocument();
    // The rail chrome and its floating launcher belong to classic mode only.
    expect(document.querySelector(".assistant-pane")).toBeNull();
    expect(screen.queryByRole("button", { name: "Open soundAr assistant" })).toBeNull();
    // The left menu survives the redesign untouched.
    expect(screen.getByRole("navigation", { name: "Create navigation" })).toBeInTheDocument();
  });

  it("leaves chat mode for any destination, including the view already selected behind the canvas", async () => {
    const navigate = vi.fn();
    const changeChatMode = vi.fn();
    render(
      <AppShell current="generate" onNavigate={navigate} theme="light" onToggleTheme={vi.fn()} system={system} runtime="tauri" features={features} assistantOpen={false} onAssistantOpenChange={vi.fn()} chatMode onChatModeChange={changeChatMode}>
        <div>Content</div>
      </AppShell>,
    );

    await screen.findByRole("heading", { name: "Hello" });
    const create = within(screen.getByRole("navigation", { name: "Create navigation" }));
    const library = within(screen.getByRole("navigation", { name: "Library navigation" }));
    // Nothing in the rail is current while the canvas covers every view.
    expect(create.getByRole("button", { name: "Generate" })).not.toHaveAttribute("aria-current");

    fireEvent.click(create.getByRole("button", { name: "Generate" }));
    expect(changeChatMode).toHaveBeenCalledWith(false);
    expect(navigate).not.toHaveBeenCalled();

    changeChatMode.mockClear();
    fireEvent.click(library.getByRole("button", { name: "Voices" }));
    expect(changeChatMode).toHaveBeenCalledWith(false);
    expect(navigate).toHaveBeenCalledWith("voices");
  });

  it("switches modes from the top bar without unmounting the assistant", async () => {
    const changeChatMode = vi.fn();
    const { rerender } = render(
      <AppShell current="generate" onNavigate={vi.fn()} theme="light" onToggleTheme={vi.fn()} system={system} runtime="tauri" features={features} assistantOpen={false} onAssistantOpenChange={vi.fn()} chatMode onChatModeChange={changeChatMode}>
        <div>Classic content</div>
      </AppShell>,
    );

    await screen.findByRole("heading", { name: "Hello" });
    fireEvent.click(screen.getByRole("button", { name: "Classic" }));
    expect(changeChatMode).toHaveBeenCalledWith(false);

    rerender(
      <AppShell current="generate" onNavigate={vi.fn()} theme="light" onToggleTheme={vi.fn()} system={system} runtime="tauri" features={features} assistantOpen onAssistantOpenChange={vi.fn()} chatMode={false} onChatModeChange={changeChatMode}>
        <div>Classic content</div>
      </AppShell>,
    );

    // Same element, re-placed by the shell grid: the docked rail now owns the conversation.
    expect(screen.getByText("Classic content")).toBeInTheDocument();
    expect(document.querySelector(".assistant-pane")).toBeInTheDocument();
    expect(document.querySelector(".assistant-canvas")).toBeNull();
    expect(screen.getByLabelText("Message soundAr assistant")).toBeInTheDocument();
    expect(document.querySelector(".app-shell")).toHaveClass("is-assistant-open");

    fireEvent.click(screen.getByRole("button", { name: "Chat" }));
    expect(changeChatMode).toHaveBeenCalledWith(true);
  });
});
