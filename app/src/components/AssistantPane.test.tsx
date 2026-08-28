import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AssistantLauncher, AssistantPane, selectAssistantArtifacts, selectAssistantJobs, videoPhaseForTool, videoProjectIdFromToolResult } from "./AssistantPane";
import { VideoIntegrationProvider } from "./video/VideoIntegrationContext";
import { createBrowserPreviewVideoService } from "../lib/videoBridge";

vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
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
