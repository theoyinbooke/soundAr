import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { CodexStatus } from "../lib/codexBridge";
import { AssistantLauncher, AssistantPane, selectAssistantArtifacts, selectAssistantJobs, videoPhaseForTool, videoProjectIdFromToolResult } from "./AssistantPane";
import { VideoIntegrationProvider } from "./video/VideoIntegrationContext";
import { createBrowserPreviewVideoService } from "../lib/videoBridge";

const codexConnection = vi.hoisted(() => ({ refresh: vi.fn() }));

vi.mock("../lib/codexBridge", async (importOriginal) => ({
  ...await importOriginal<typeof import("../lib/codexBridge")>(),
  refreshCodexConnection: codexConnection.refresh,
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

const connectedCodex: CodexStatus = {
  available: true,
  connected: true,
  path: "/usr/local/bin/codex",
  version: "codex-cli test",
  studio_root: "/home/studio/.soundAr",
};

beforeEach(() => {
  codexConnection.refresh.mockReset();
  codexConnection.refresh.mockResolvedValue(connectedCodex);
});
afterEach(cleanup);

describe("AssistantPane", () => {
  it("groups shared video tools into high-level phases and resolves structured project results", () => {
    expect(videoPhaseForTool("analyze_video")).toBe("analyze");
    expect(videoPhaseForTool("soundar/export_video")).toBe("export");
    expect(videoPhaseForTool("mcp__soundar__render_video_preview")).toBe("preview");
    expect(videoProjectIdFromToolResult({ output: [{ type: "text", text: JSON.stringify({ artifact: { role: "master", project_id: "video-project-7" } }) }] })).toBe("video-project-7");
  });

  it("renders the assembled video master prominently and keeps project assets secondary", async () => {
    const onOpenProject = vi.fn();
    render(<VideoIntegrationProvider service={createBrowserPreviewVideoService()} onOpenProject={onOpenProject}>
      <AssistantPane open onClose={vi.fn()} />
    </VideoIntegrationProvider>);

    const composer = await screen.findByRole("textbox", { name: "Message soundAr assistant" });
    await userEvent.type(composer, "Render a short portrait reel with calm captions{enter}");
    const progress = await screen.findByRole("region", { name: "Video production progress" }, { timeout: 2_000 });
    expect(within(progress).getByText("Video production complete")).toBeVisible();
    expect(within(progress).getByText("Export")).toBeVisible();
    const master = await screen.findByRole("article", { name: "Final video master: Creator update · Portrait master" });
    expect(within(master).getByLabelText("Play Creator update · Portrait master")).toBeInstanceOf(HTMLVideoElement);
    expect(within(master).getByRole("link", { name: "Download Creator update · Portrait master" })).toHaveAttribute("download", "creator-update-master-portrait-master.mp4");
    const secondaryAssets = within(master).getByText("Project assets").closest("details");
    expect(secondaryAssets).not.toHaveAttribute("open");
    await userEvent.click(within(master).getByRole("button", { name: "Open project" }));
    expect(onOpenProject).toHaveBeenCalledWith("creator-update-master");
  });

  it("restores the current playable video and compact phases when a saved task resumes", async () => {
    const service = createBrowserPreviewVideoService();
    render(<VideoIntegrationProvider service={service} onOpenProject={vi.fn()}>
      <AssistantPane open onClose={vi.fn()} />
    </VideoIntegrationProvider>);

    await screen.findByRole("textbox", { name: "Message soundAr assistant" });
    await userEvent.click(screen.getByRole("button", { name: "Conversation history" }));
    await userEvent.click(await screen.findByRole("menuitem", { name: /Saved video production/i }));

    const phases = await screen.findByRole("region", { name: "Video production progress" });
    expect(within(phases).getByText("Video production complete")).toBeVisible();
    expect(within(phases).getByText("Export")).toBeVisible();
    const master = await screen.findByRole("article", {
      name: "Final video master: Creator update · Portrait master",
    });
    expect(within(master).getByLabelText("Play Creator update · Portrait master")).toBeInstanceOf(HTMLVideoElement);
    expect(within(master).getByRole("link", { name: "Download Creator update · Portrait master" }))
      .toHaveAttribute("download", "creator-update-master-portrait-master.mp4");
  });

  it("restores the exact playable preview when a saved task has no final master", async () => {
    const service = createBrowserPreviewVideoService();
    await service.renderVideoPreview("creator-update");
    render(<VideoIntegrationProvider service={service} onOpenProject={vi.fn()}>
      <AssistantPane open onClose={vi.fn()} />
    </VideoIntegrationProvider>);

    await screen.findByRole("textbox", { name: "Message soundAr assistant" });
    await userEvent.click(screen.getByRole("button", { name: "Conversation history" }));
    await userEvent.click(await screen.findByRole("menuitem", { name: /Saved video preview/i }));

    const preview = await screen.findByRole("article", {
      name: "Video preview: Creator update · Reel draft preview",
    });
    expect(within(preview).getByLabelText("Play Creator update · Reel draft preview"))
      .toBeInstanceOf(HTMLVideoElement);
    expect(within(preview).getByRole("link", { name: "Download Creator update · Reel draft preview" }))
      .toHaveAttribute("download", "creator-update-preview.mp4");
    expect(screen.queryByText("Final video master")).not.toBeInTheDocument();
  });

  it("does not substitute a current master when a stale saved output falls back to its project", async () => {
    render(<VideoIntegrationProvider service={createBrowserPreviewVideoService()} onOpenProject={vi.fn()}>
      <AssistantPane open onClose={vi.fn()} />
    </VideoIntegrationProvider>);

    await screen.findByRole("textbox", { name: "Message soundAr assistant" });
    await userEvent.click(screen.getByRole("button", { name: "Conversation history" }));
    await userEvent.click(await screen.findByRole("menuitem", { name: /Saved unavailable output/i }));

    expect(await screen.findByRole("region", { name: "Video production progress" })).toBeVisible();
    expect(screen.queryByRole("article", { name: /Final video master|Video preview/i })).not.toBeInTheDocument();
  });

  it("shows only the final master for project workflows and one clip for single requests", () => {
    const base = { voice: "Default", text: "", generation_kind: "speech" as const, audio_path: "/managed/audio.wav", sample_rate: 24000, duration_seconds: 1, inference_seconds: 0, rtf: 0, vram_peak_mb: 0, waveform: [], created_at: "2026-08-27T18:00:00Z", preview: false };
    const history = [
      { ...base, id: "master", title: "Project master", model_id: "soundar/project-master", engine: "finishing" },
      { ...base, id: "chapter-2", title: "Chapter 2", model_id: "tts", engine: "breeze" },
      { ...base, id: "chapter-1", title: "Chapter 1", model_id: "tts", engine: "breeze" },
    ];
    expect(selectAssistantArtifacts(history, new Set(), "project").map((item) => item.id)).toEqual(["master"]);
    expect(selectAssistantArtifacts(history, new Set(), "single").map((item) => item.id)).toEqual(["chapter-2"]);
  });

  it("tracks only newly created active audio jobs for progressive feedback", () => {
    const base = { kind: "synthesis", progress: 0.8, stage: "decoding" as const, attempt: 1, created_at: "2026-08-27T18:00:00Z", updated_at: "2026-08-27T18:00:01Z" };
    const jobs = [
      { ...base, id: "new-speech", status: "running" as const },
      { ...base, id: "old-speech", status: "running" as const },
      { ...base, id: "finished", status: "completed" as const },
      { ...base, id: "model-load", kind: "model-load", status: "running" as const },
    ];
    expect(selectAssistantJobs(jobs, new Set(["old-speech"])).map((job) => job.id)).toEqual(["new-speech"]);
  });

  it("opens from a restrained floating action", async () => {
    const onClick = vi.fn();
    render(<AssistantLauncher onClick={onClick} />);
    await userEvent.click(screen.getByRole("button", { name: "Open soundAr assistant" }));
    expect(onClick).toHaveBeenCalledOnce();
  });

  it("automatically resolves Codex on the first open without showing a detection warning", async () => {
    render(<AssistantPane open onClose={vi.fn()} />);

    expect(screen.getByText("Connecting to Codex…")).toBeVisible();
    expect(screen.queryByText("Codex CLI not detected")).not.toBeInTheDocument();
    expect(await screen.findByRole("button", { name: /GPT-5.6-Sol/i })).toBeInTheDocument();
    expect(codexConnection.refresh).toHaveBeenCalledOnce();
  });

  it("refreshes a stale unavailable result before painting a reopened pane", async () => {
    let resolveReopen!: (status: CodexStatus) => void;
    const reopened = new Promise<CodexStatus>((resolve) => { resolveReopen = resolve; });
    codexConnection.refresh
      .mockResolvedValueOnce({ available: false, connected: false, message: "Codex CLI was not found." })
      .mockImplementationOnce(() => reopened);
    const onClose = vi.fn();
    const view = render(<AssistantPane open onClose={onClose} />);
    expect(await screen.findByText("Codex CLI not detected")).toBeVisible();

    view.rerender(<AssistantPane open={false} onClose={onClose} />);
    view.rerender(<AssistantPane open onClose={onClose} />);

    expect(screen.queryByText("Codex CLI not detected")).not.toBeInTheDocument();
    expect(screen.getByText("Connecting to Codex…")).toBeVisible();
    await act(async () => resolveReopen(connectedCodex));
    expect(await screen.findByRole("button", { name: /GPT-5.6-Sol/i })).toBeInTheDocument();
    expect(codexConnection.refresh).toHaveBeenCalledTimes(2);
  });

  it("loads Codex controls and completes the preview conversation flow", async () => {
    render(<AssistantPane open onClose={vi.fn()} />);
    expect(await screen.findByRole("complementary", { name: "soundAr assistant" })).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: /GPT-5.6-Sol/i })).toBeInTheDocument();
    const composer = screen.getByRole("textbox", { name: "Message soundAr assistant" });
    await userEvent.type(composer, "Create a short ambient cue{enter}");
    expect(screen.getByText("Create a short ambient cue")).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText(/Activity complete · 2 actions/i)).toBeInTheDocument(), { timeout: 2_000 });
    expect(screen.getByText(/working brief/i)).toBeInTheDocument();
    expect(screen.getByRole("list", { name: "Current plan" })).toBeInTheDocument();
    expect(screen.getByText("Get studio state")).toBeInTheDocument();
    expect(screen.getByText("Queue music generation")).toBeInTheDocument();
  });

  it("keeps full access explicit and user selectable", async () => {
    render(<AssistantPane open onClose={vi.fn()} />);
    await screen.findByRole("button", { name: /Studio access/i });
    await userEvent.click(screen.getByRole("button", { name: /Studio access/i }));
    await userEvent.click(screen.getByRole("menuitemradio", { name: /Full access/i }));
    expect(screen.getByRole("button", { name: /Full access/i })).toBeInTheDocument();
  });

  it("keeps context controls left and reasoning beside the send action", async () => {
    render(<AssistantPane open onClose={vi.fn()} />);
    const model = await screen.findByRole("button", { name: /GPT-5.6-Sol/i });
    const access = screen.getByRole("button", { name: /Studio access/i });
    const effort = screen.getByRole("button", { name: /Low/i });
    const send = screen.getByRole("button", { name: "Send message" });

    expect(model.compareDocumentPosition(access) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(access.compareDocumentPosition(effort) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(effort.compareDocumentPosition(send) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();

    await userEvent.click(effort);
    expect(screen.getByRole("menu", { name: "Reasoning effort" })).toHaveClass("picker-effort");
  });
});
