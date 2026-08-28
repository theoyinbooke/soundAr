import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fallbackBootstrap, seedVoices } from "../data";
import { createBrowserPreviewVideoService } from "../lib/videoBridge";
import { VideoStudioView } from "./VideoStudioView";

afterEach(cleanup);

describe("VideoStudioView", () => {
  it("renders a playable and downloadable final master after preview and export", async () => {
    const user = userEvent.setup();
    render(<VideoStudioView service={createBrowserPreviewVideoService()} />);

    await user.click(await screen.findByRole("button", { name: /Creator update · Reel draft/i }));
    await user.click(await screen.findByRole("button", { name: "Render preview" }));
    await user.click(await screen.findByRole("button", { name: "Export video" }));

    expect(await screen.findByRole("heading", { name: "Export complete" })).toBeVisible();
    expect(screen.getByLabelText("Final video: Creator update · Portrait master")).toHaveAttribute("src", expect.stringMatching(/^data:video\/mp4;base64,/));
    expect(screen.getByRole("link", { name: "Download master" })).toHaveAttribute("download", "creator-update-portrait-master.mp4");
    await user.click(screen.getByRole("button", { name: "Publish package" }));
    expect(await screen.findByRole("link", { name: "Download package" })).toHaveAttribute("download", "creator-update-publish-package.zip");
  });

  it("persists inspector changes through the shared revision service", async () => {
    const user = userEvent.setup();
    const service = createBrowserPreviewVideoService();
    const revise = vi.spyOn(service, "reviseVideo");
    render(<VideoStudioView service={service} />);

    await user.click(await screen.findByRole("button", { name: /Creator update · Reel draft/i }));
    await user.click(await screen.findByRole("tab", { name: "Captions" }));
    await user.selectOptions(screen.getByRole("combobox", { name: "Caption style" }), "calm");
    const save = screen.getByRole("button", { name: "Save scene changes" });
    expect(save).toBeEnabled();
    await user.click(save);
    await screen.findByText(/Saved Hook: where I’ve been/i);

    const project = await service.getVideoProject("creator-update");
    expect(project.manifest.scenes[0].caption_style).toBe("calm");
    expect(project.manifest.revisions).toHaveLength(1);
    expect(revise.mock.calls[0]?.[0].scene_patch).not.toHaveProperty("voice_id");
    expect(revise.mock.calls[0]?.[0].scene_patch).not.toHaveProperty("model_id");
  });

  it("selects a complete consent-safe narration route with keyboard tab navigation", async () => {
    const user = userEvent.setup();
    const service = createBrowserPreviewVideoService();
    const revise = vi.spyOn(service, "reviseVideo");
    render(<VideoStudioView service={service} bootstrap={fallbackBootstrap} voices={seedVoices} />);

    await user.click(await screen.findByRole("button", { name: /Creator update · Reel draft/i }));
    await waitFor(() => expect(screen.getByRole("heading", { name: "Creator update · Reel draft" })).toHaveFocus());
    const layoutTab = screen.getByRole("tab", { name: "Layout" });
    layoutTab.focus();
    await user.keyboard("{ArrowRight}{ArrowRight}");
    expect(screen.getByRole("tab", { name: "Audio" })).toHaveFocus();
    await user.selectOptions(screen.getByRole("combobox", { name: "Narration model" }), "hexgrad/Kokoro-82M");
    expect(screen.getByRole("combobox", { name: "Narration voice" })).toHaveValue("studio-neutral");
    await user.selectOptions(screen.getByRole("combobox", { name: "Narration language" }), "en-gb");
    await user.click(screen.getByRole("button", { name: "Save scene changes" }));

    expect(revise).toHaveBeenCalledWith(expect.objectContaining({
      scene_patch: expect.objectContaining({
        model_id: "hexgrad/Kokoro-82M",
        voice_id: "studio-neutral",
        speaker: "studio-neutral",
        language: "en-gb",
      }),
    }));
  });

  it("saves an explicit normalized manual frame instead of a preset crop", async () => {
    const user = userEvent.setup();
    const service = createBrowserPreviewVideoService();
    const revise = vi.spyOn(service, "reviseVideo");
    render(<VideoStudioView service={service} />);

    await user.click(await screen.findByRole("button", { name: /Creator update · Reel draft/i }));
    await user.selectOptions(screen.getByRole("combobox", { name: "Scene crop mode" }), "manual");
    expect(screen.getByRole("slider", { name: "Manual crop focus X" })).toBeVisible();
    expect(screen.getByRole("slider", { name: "Manual crop focus Y" })).toBeVisible();
    expect(screen.getByRole("slider", { name: "Manual crop zoom" })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Save scene changes" }));

    expect(revise).toHaveBeenCalledWith(expect.objectContaining({
      scene_patch: expect.objectContaining({
        crop_mode: "manual",
        crop_rect: { x_bp: 3418, y_bp: 0, width_bp: 3164, height_bp: 10000 },
      }),
    }));
  });

  it("resumes the exact durable analysis job after reopening instead of creating a retry", async () => {
    const user = userEvent.setup();
    const base = createBrowserPreviewVideoService();
    const draft = await base.getVideoProject("creator-update");
    draft.status = "analyzing";
    draft.workflow_job = {
      id: "analysis-job-durable-1", project_id: draft.id, phase: "analyze", status: "failed",
      progress: 0.46, title: "Analyze source", detail: "Interrupted during transcription",
      durable: true, created_at: draft.created_at, updated_at: draft.updated_at,
      error: "Application restarted",
    };
    draft.recoverable_job = draft.workflow_job;
    const completed = structuredClone(draft);
    completed.status = "review";
    completed.workflow_job = undefined;
    completed.recoverable_job = undefined;
    let resumed = false;
    const resumeVideoJob = vi.fn(async (jobId: string) => {
      expect(jobId).toBe("analysis-job-durable-1");
      resumed = true;
      return { ...draft.workflow_job!, status: "running" as const, detail: "Resuming source-clock transcription" };
    });
    const service = {
      ...base,
      getVideoProject: vi.fn(async () => structuredClone(resumed ? completed : draft)),
      resumeVideoJob,
      analyzeVideo: vi.fn(base.analyzeVideo),
    };
    render(<VideoStudioView service={service} initialProjectId="creator-update" />);

    await user.click(await screen.findByRole("button", { name: "Resume analysis" }));
    expect(resumeVideoJob).toHaveBeenCalledOnce();
    expect(service.analyzeVideo).not.toHaveBeenCalled();
    expect(await screen.findByRole("heading", { name: "Review candidate clips" }, { timeout: 3_000 })).toBeVisible();
  });

  it("discovers and resumes the exact prompt parent after restart without creating another project", async () => {
    const user = userEvent.setup();
    const base = createBrowserPreviewVideoService();
    const draft = await base.getVideoProject("creator-update");
    draft.status = "draft";
    draft.manifest.source = {
      ...draft.manifest.source,
      kind: "prompt",
      display_name: "Prompt brief",
      preview_url: undefined,
    };
    draft.workflow_job = {
      id: "prompt-parent-durable-1", project_id: draft.id, phase: "source", status: "failed",
      progress: 0.08, title: "Preparing source", detail: "Application restarted before narration generation",
      durable: true, created_at: draft.created_at, updated_at: draft.updated_at,
      error: "Application restarted",
    };
    draft.recoverable_job = draft.workflow_job;
    const completed = structuredClone(draft);
    completed.status = "editing";
    completed.workflow_job = undefined;
    completed.recoverable_job = undefined;
    let resumed = false;
    const resumeVideoJob = vi.fn(async (jobId: string) => {
      expect(jobId).toBe("prompt-parent-durable-1");
      resumed = true;
      return { ...draft.workflow_job!, status: "running" as const, detail: "Reusing the durable prompt workflow" };
    });
    const service = {
      ...base,
      getVideoProject: vi.fn(async () => structuredClone(resumed ? completed : draft)),
      resumeVideoJob,
      createVideoProject: vi.fn(base.createVideoProject),
      analyzeVideo: vi.fn(base.analyzeVideo),
    };
    render(<VideoStudioView service={service} initialProjectId="creator-update" />);

    expect(await screen.findByRole("heading", { name: `Preparing ${draft.name}` })).toBeVisible();
    expect(screen.queryByLabelText("Playable low-resolution source proxy")).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Resume video creation" }));
    expect(resumeVideoJob).toHaveBeenCalledOnce();
    expect(service.createVideoProject).not.toHaveBeenCalled();
    expect(service.analyzeVideo).not.toHaveBeenCalled();
    expect(await screen.findByRole("heading", { name: draft.name }, { timeout: 3_000 })).toBeVisible();
  });
});
