import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fallbackBootstrap, seedVoices } from "../data";
import { createBrowserPreviewVideoService } from "../lib/videoBridge";
import type { VideoJob, VideoProject, VideoStudioService } from "../types/video";
import { VideoStudioView } from "./VideoStudioView";

afterEach(() => {
  cleanup();
  window.localStorage.removeItem("soundar.video-studio.timeline-mode");
});

describe("VideoStudioView", () => {
  it("shows each line's take state and narrates only the lines that need it", async () => {
    const user = userEvent.setup();
    const base = createBrowserPreviewVideoService();
    const narrated: { turnIds: string[]; draft?: boolean }[] = [];
    const project = await base.getVideoProject("creator-update");
    // One performed line, one still standing in, one never read.
    project.manifest.cast = [
      { id: "narrator", name: "NARRATOR", display_name: "Narrator", voice_id: "af-heart", model_id: "kokoro-82m", language: "en-US", delivery: { rate_milli: 1000, pitch_milli: 0, energy_milli: 1000 }, created_at: "2026-01-01T00:00:00Z" },
    ];
    project.manifest.dialogue = [
      { id: "turn-a", scene_id: null, order: 0, character_id: "narrator", text: "The harmattan came early.", direction: null, source_line: 1, revision: 1, narrated: true, draft: false },
      { id: "turn-b", scene_id: null, order: 1, character_id: "narrator", text: "She waited.", direction: null, source_line: 2, revision: 1, narrated: true, draft: true },
      { id: "turn-c", scene_id: null, order: 2, character_id: "narrator", text: "She said nothing at all.", direction: "quiet", source_line: 3, revision: 1, narrated: false, draft: false },
    ];
    const service: VideoStudioService = {
      ...base,
      getVideoProject: async () => project,
      narrateTurns: async (_projectId, turnIds, draft) => {
        narrated.push({ turnIds, draft });
        return project;
      },
    };

    // Pronunciation, score, and sound design are shown beside the script so an episode's state is
    // in one place rather than spread across surfaces the user has to know to look for.
    project.manifest.lexicon = [
      { id: "rule-adaeze", scope: "project", match_text: "Adaeze", replacement: "Ah-DAH-eh-zeh", matching: "word", created_at: "2026-01-01T00:00:00Z" },
    ];
    project.manifest.music_cues = [
      { id: "cue-outro", role: "outro", anchor: { kind: "after_final_turn" }, target_duration_ms: 20_000, direction: "warm, resolving", gain_db: -6, fade_in_ms: 500, fade_out_ms: 2_000, needs_generation: true, created_at: "2026-01-01T00:00:00Z" },
    ];

    render(<VideoStudioView service={service} />);
    await user.click(await screen.findByRole("button", { name: /From interview source/i }));
    await user.click(await screen.findByRole("tab", { name: "Cast" }));

    expect(await screen.findByText(/Adaeze → Ah-DAH-eh-zeh/)).toBeVisible();
    // A cue that has not been composed says so rather than looking finished.
    expect(screen.getByText(/Not composed yet/)).toBeVisible();

    // The three states are named rather than left for the reader to infer.
    expect(await screen.findByText("Not narrated")).toBeVisible();
    expect(screen.getByText("Draft take")).toBeVisible();
    expect(screen.getByText("Performed")).toBeVisible();
    // A stand-in blocks publication, and the panel says so.
    expect(screen.getByText(/still draft takes/i)).toBeVisible();

    await user.click(screen.getByRole("button", { name: /Narrate 1 remaining/i }));
    await waitFor(() => expect(narrated).toHaveLength(1));
    // Only the unperformed line is read; the finished and draft lines are left alone.
    expect(narrated[0]).toEqual({ turnIds: ["turn-c"], draft: false });
  });

  it("routes a stale downstream preview failure back to source analysis", async () => {
    const user = userEvent.setup();
    render(<VideoStudioView service={createBrowserPreviewVideoService()} />);

    await user.click(await screen.findByRole("button", {
      name: /From interview source to a focused portrait story with a concise opening/i,
    }));

    expect(await screen.findByRole("heading", { name: "Analyze this source" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Analyze source" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: /Resume analysis/i })).not.toBeInTheDocument();
    expect(screen.queryByText(/video\.reviewed_scenes_required/i)).not.toBeInTheDocument();
    const title = screen.getByRole("heading", {
      name: "From interview source to a focused portrait story with a concise opening",
    });
    expect(title).toHaveAttribute("title", "From interview source to a focused portrait story with a concise opening");
  });

  it("guides a source-only project through Analyze, Review, and preview readiness", async () => {
    const user = userEvent.setup();
    const base = createBrowserPreviewVideoService();
    const editorTemplate = await base.getVideoProject("creator-update");
    const reviewTemplate = await base.getVideoProject("product-demo");
    const renderedTemplate = await base.renderVideoPreview("creator-update");
    const sourceOnly = structuredClone(editorTemplate);
    sourceOnly.name = "Uploaded interview · Reel draft";
    sourceOnly.status = "editing";
    sourceOnly.duration_ms = 0;
    sourceOnly.scene_count = 0;
    sourceOnly.manifest.source = {
      ...sourceOnly.manifest.source,
      kind: "local-video",
      display_name: "uploaded-interview.mp4",
      provenance: "User-selected local media",
    };
    sourceOnly.manifest.transcript = [];
    sourceOnly.manifest.caption_pages = [];
    sourceOnly.manifest.candidates = [];
    sourceOnly.manifest.scenes = [];
    sourceOnly.manifest.timeline = {
      duration_ms: 0,
      source_clock_duration_ms: sourceOnly.manifest.source.duration_ms,
      tracks: sourceOnly.manifest.timeline.tracks.map((track) => ({ ...track, items: [] })),
    };

    const analyzed = structuredClone(sourceOnly);
    analyzed.status = "review";
    analyzed.manifest.transcript = structuredClone(reviewTemplate.manifest.transcript);
    analyzed.manifest.candidates = structuredClone(reviewTemplate.manifest.candidates);

    const planned = structuredClone(editorTemplate);
    planned.name = sourceOnly.name;
    planned.manifest.source = structuredClone(sourceOnly.manifest.source);
    const previewed = structuredClone(planned);
    previewed.manifest.artifacts.push(
      structuredClone(renderedTemplate.manifest.artifacts.find((artifact) => artifact.role === "preview")!),
    );

    let current: VideoProject = sourceOnly;
    const completedJob = (phase: VideoJob["phase"], title: string): VideoJob => ({
      id: `${phase}-job`, project_id: current.id, phase, status: "completed", progress: 1,
      title, detail: `${title} complete`, durable: true,
      created_at: current.created_at, updated_at: current.updated_at,
    });
    const analyzeVideo: VideoStudioService["analyzeVideo"] = vi.fn(async (_projectId, onProgress) => {
      onProgress?.({ job: completedJob("analyze", "Analyzing source") });
      current = structuredClone(analyzed);
      return structuredClone(current);
    });
    const planVideo: VideoStudioService["planVideo"] = vi.fn(async (_projectId, selectedIds) => {
      expect(selectedIds).toEqual(["clip-1", "clip-2", "clip-4"]);
      current = structuredClone(planned);
      return structuredClone(current);
    });
    const renderVideoPreview: VideoStudioService["renderVideoPreview"] = vi.fn(async (_projectId, onProgress) => {
      onProgress?.({ job: completedJob("preview", "Rendering preview") });
      current = structuredClone(previewed);
      return structuredClone(current);
    });
    const service: VideoStudioService = {
      ...base,
      listVideoProjects: vi.fn(async () => []),
      getVideoProject: vi.fn(async () => structuredClone(current)),
      analyzeVideo,
      planVideo,
      renderVideoPreview,
    };

    render(<VideoStudioView service={service} initialProjectId={sourceOnly.id} />);

    expect(await screen.findByRole("heading", { name: sourceOnly.name })).toBeVisible();
    let steps = screen.getByRole("navigation", { name: "Video production progress" });
    expect(within(steps).getByText("Source").closest("span")).toHaveAttribute("data-status", "complete");
    expect(within(steps).getByText("Analyze").closest("span")).toHaveAttribute("aria-current", "step");
    expect(within(steps).getByText("Review").closest("span")).toHaveAttribute("data-status", "upcoming");
    expect(screen.queryByRole("button", { name: "Render preview" })).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Analyze source" }));
    expect(analyzeVideo).toHaveBeenCalledWith(sourceOnly.id, expect.any(Function));
    expect(await screen.findByRole("heading", { name: "Review candidate clips" })).toBeVisible();
    steps = screen.getByRole("navigation", { name: "Video production progress" });
    expect(within(steps).getByText("Analyze").closest("span")).toHaveAttribute("data-status", "complete");
    expect(within(steps).getByText("Review").closest("span")).toHaveAttribute("aria-current", "step");

    await user.click(screen.getByRole("button", { name: "Plan selected clips" }));
    expect(planVideo).toHaveBeenCalledOnce();
    expect(await screen.findByRole("button", { name: "Render preview" })).toBeEnabled();
    steps = screen.getByRole("navigation", { name: "Video production progress" });
    expect(within(steps).getByText("Review").closest("span")).toHaveAttribute("data-status", "complete");
    expect(within(steps).getByText("Preview").closest("span")).toHaveAttribute("aria-current", "step");

    await user.click(screen.getByRole("button", { name: "Render preview" }));
    expect(renderVideoPreview).toHaveBeenCalledOnce();
    expect(await screen.findByRole("button", { name: "Export video" })).toBeEnabled();
    steps = screen.getByRole("navigation", { name: "Video production progress" });
    expect(within(steps).getByText("Preview").closest("span")).toHaveAttribute("data-status", "complete");
    expect(within(steps).getByText("Export").closest("span")).toHaveAttribute("aria-current", "step");
  });

  it("renders a playable and downloadable final master after preview and export", async () => {
    const user = userEvent.setup();
    render(<VideoStudioView service={createBrowserPreviewVideoService()} />);

    await user.click(await screen.findByRole("button", { name: /Creator update · Reel draft/i }));
    await user.click(await screen.findByRole("button", { name: "Render preview" }));
    await user.click(await screen.findByRole("button", { name: "Export video" }));

    expect(await screen.findByText("Exported")).toBeVisible();
    expect(screen.getByLabelText("Final video: Creator update · Portrait master")).toHaveAttribute("src", expect.stringMatching(/^data:video\/mp4;base64,/));
    // Browser preview holds no local export, so saving is offered but unavailable.
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    expect(screen.queryByRole("heading", { name: "Export complete" })).not.toBeInTheDocument();
    const master = screen.getByRole("main", { name: "Final master" });
    expect(within(master).queryByRole("button")).not.toBeInTheDocument();
    expect(within(master).queryByRole("link")).not.toBeInTheDocument();
    expect(screen.getByText("Master export ready.").closest('[role="status"]')).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Publish package" }));
    expect(await screen.findByRole("button", { name: "Save package" })).toBeDisabled();
    expect(screen.queryByText(/NVENC|Cache reuse|AAC · 48 kHz/i)).not.toBeInTheDocument();
    expect(screen.getByText("Publish package ready.").closest('[role="status"]')).toBeVisible();
  });

  it("gives the export screen a playable master without a duplicate meta strip", async () => {
    const user = userEvent.setup();
    render(<VideoStudioView service={createBrowserPreviewVideoService()} />);

    await user.click(await screen.findByRole("button", { name: /Creator update · Reel master/i }));
    const master = await screen.findByLabelText(/^Final video: /);
    // The controls must be reachable: an overlay poster on top of the video hid them entirely.
    expect(master).toHaveAttribute("controls");
    expect(document.querySelector(".video-opening-poster")).toBeNull();
    // The strip under the player repeated the export receipt beside it and cost the portrait height.
    expect(document.querySelector(".video-master-card > footer")).toBeNull();
    const receipt = screen.getByText("Export receipt").closest("aside")!;
    expect(within(receipt).getByText("Revision")).toBeInTheDocument();
    expect(within(receipt).getByText("Saved")).toBeInTheDocument();
    expect(within(receipt).getByText("Checksum")).toBeInTheDocument();
  });

  it("keeps responsive export actions keyboard accessible in the top toolbar", async () => {
    const user = userEvent.setup();
    render(<VideoStudioView service={createBrowserPreviewVideoService()} />);

    await user.click(await screen.findByRole("button", { name: /Creator update · Reel master/i }));
    const trigger = document.querySelector<HTMLButtonElement>('[aria-label="More export actions"]');
    expect(trigger).not.toBeNull();
    trigger!.closest<HTMLElement>(".video-export-overflow")!.style.display = "block";
    await user.click(trigger!);
    // Browser preview owns no local export, so both filesystem actions stay disabled and focus
    // lands on the first action that can actually run.
    expect(screen.getByRole("menuitem", { name: "Save master" })).toBeDisabled();
    expect(screen.getByRole("menuitem", { name: "Open output folder" })).toBeDisabled();
    await waitFor(() => expect(screen.getByRole("menuitem", { name: "Open project" })).toHaveFocus());
    fireEvent.keyDown(screen.getByRole("menu", { name: "More export actions" }), { key: "ArrowDown" });
    expect(screen.getByRole("menuitem", { name: "Publish package" })).toHaveFocus();
    await user.keyboard("{Escape}");
    expect(trigger).toHaveFocus();
    expect(screen.queryByRole("menu", { name: "More export actions" })).not.toBeInTheDocument();
  });

  it("persists inspector changes through the shared revision service", async () => {
    const user = userEvent.setup();
    const service = createBrowserPreviewVideoService();
    const revise = vi.spyOn(service, "reviseVideo");
    render(<VideoStudioView service={service} />);

    await user.click(await screen.findByRole("button", { name: /Creator update · Reel draft/i }));
    await user.click(await screen.findByRole("tab", { name: "Captions" }));
    await user.click(screen.getByRole("radio", { name: /Calm/ }));
    fireEvent.change(screen.getByRole("slider", { name: "Caption horizontal position" }), { target: { value: "1000" } });
    const save = screen.getByRole("button", { name: "Save scene changes" });
    expect(save).toBeEnabled();
    await user.click(save);
    await screen.findByText(/Saved Hook: where I’ve been/i);

    const project = await service.getVideoProject("creator-update");
    expect(project.manifest.scenes[0].caption_style).toBe("calm");
    expect(project.manifest.scenes[0].caption_bounds).toEqual({ x_bp: 1000, y_bp: 7350, width_bp: 8400, height_bp: 1500 });
    expect(project.manifest.revisions).toHaveLength(1);
    expect(revise.mock.calls[0]?.[0].scene_patch).not.toHaveProperty("voice_id");
    expect(revise.mock.calls[0]?.[0].scene_patch).not.toHaveProperty("model_id");
    expect(revise.mock.calls[0]?.[0].scene_patch).toMatchObject({ caption_bounds: { x_bp: 1000, y_bp: 7350, width_bp: 8400, height_bp: 1500 } });
  });

  it("keeps the caption inspector and resizable canvas controls available with the assistant open", async () => {
    const user = userEvent.setup();
    render(<VideoStudioView service={createBrowserPreviewVideoService()} assistantOpen />);

    await user.click(await screen.findByRole("button", { name: /Creator update · Reel draft/i }));
    const inspector = screen.getByRole("complementary", { name: "Scene inspector" });
    expect(inspector).toBeVisible();
    expect(within(inspector).getByRole("radiogroup", { name: "Caption style" })).toBeVisible();
    expect(screen.getByRole("separator", { name: "Resize scenes panel" })).toHaveAttribute("aria-valuenow", "190");
    expect(screen.getByRole("separator", { name: "Resize scene inspector" })).toHaveAttribute("aria-valuenow", "270");

    await user.keyboard("{Tab}");
    await user.click(screen.getByRole("button", { name: "Add" }));
    expect(screen.getByRole("menu", { name: "Add element" })).toBeVisible();
    expect(screen.getByRole("menuitem", { name: /Edit captions/ })).toBeVisible();
  });

  it("adds a picker-confirmed image to the selected scene through the shared durable service", async () => {
    const user = userEvent.setup();
    const service = createBrowserPreviewVideoService();
    const chooseVideoVisualAsset = vi.spyOn(service, "chooseVideoVisualAsset");
    const addVideoVisualAsset = vi.spyOn(service, "addVideoVisualAsset");
    const editVideoTimeline = vi.spyOn(service, "editVideoTimeline");
    render(<VideoStudioView service={service} assistantOpen />);

    await user.click(await screen.findByRole("button", { name: /Creator update · Reel draft/i }));
    await user.click(screen.getByRole("button", { name: "Add" }));
    await user.click(screen.getByRole("menuitem", { name: /Add image/ }));

    await waitFor(() => expect(addVideoVisualAsset).toHaveBeenCalledOnce());
    expect(chooseVideoVisualAsset).toHaveBeenCalledWith({
      project_id: "creator-update",
      expected_revision: 0,
      expected_version_id: "creator-update-v1",
    });
    expect(addVideoVisualAsset.mock.calls[0]?.[0]).toMatchObject({
      project_id: "creator-update",
      expected_revision: 0,
      expected_version_id: "creator-update-v1",
      actor: "desktop-ui",
      origin: { kind: "user_selected", receipt_id: "visual-receipt-1" },
      scene_id: "scene-clip-1",
      range: { start_us: 0, end_us: 18_000_000 },
      fit: "contain",
      z_index: 10,
      motion: {
        start_bounds: { x_bp: 0, y_bp: 0, width_bp: 10_000, height_bp: 10_000 },
        end_bounds: { x_bp: 0, y_bp: 0, width_bp: 10_000, height_bp: 10_000 },
        start_opacity_milli: 1_000,
        end_opacity_milli: 1_000,
        start_rotation_milli_degrees: 0,
        end_rotation_milli_degrees: 0,
        easing: "ease_in_out",
      },
      transition_in_us: 300_000,
      transition_out_us: 300_000,
    });
    expect(addVideoVisualAsset.mock.calls[0]?.[0]).not.toHaveProperty("source_path");
    expect(addVideoVisualAsset.mock.calls[0]?.[0].operation_id).toMatch(/^visual-/);
    expect(await screen.findByRole("group", { name: "Visuals track" })).toBeVisible();
    expect(screen.getByRole("region", { name: "Video timeline" })).toHaveAttribute("data-track-count", "5");
    expect(screen.getByRole("tab", { name: "Visual" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByLabelText("Selected image layer")).toBeVisible();
    expect(document.querySelector("[data-visual-layer-id]")).toBeVisible();
    expect(screen.getAllByText(/added\. Preview and export will refresh/i).length).toBeGreaterThan(0);

    fireEvent.keyDown(screen.getByRole("button", { name: "Select image layer 1" }), { key: "ArrowLeft", altKey: true });
    await waitFor(() => expect(editVideoTimeline).toHaveBeenCalledOnce());
    expect(editVideoTimeline.mock.calls[0]?.[0]).toMatchObject({
      expected_revision: 1,
      base_version_id: "creator-update-v2",
      operations: [{
        type: "update_visual_layer",
        scene_id: "scene-clip-1",
        range: { start_us: 0, end_us: 18_000_000 },
        fit: "contain",
        crop: null,
        z_index: 10,
        motion: {
          start_bounds: { x_bp: 0, y_bp: 0, width_bp: 9_900, height_bp: 9_900 },
          end_bounds: { x_bp: 0, y_bp: 0, width_bp: 9_900, height_bp: 9_900 },
        },
        transition_in_us: 300_000,
        transition_out_us: 300_000,
      }],
    });
    expect((await screen.findAllByText(/Place image saved/i)).some((element) => element.matches(".video-timeline-feedback"))).toBe(true);
  });

  it("treats a cancelled native visual receipt as a no-op", async () => {
    const user = userEvent.setup();
    const base = createBrowserPreviewVideoService();
    const chooseVideoVisualAsset = vi.fn<VideoStudioService["chooseVideoVisualAsset"]>(async () => null);
    const addVideoVisualAsset = vi.spyOn(base, "addVideoVisualAsset");
    render(<VideoStudioView service={{ ...base, chooseVideoVisualAsset }} />);

    await user.click(await screen.findByRole("button", { name: /Creator update · Reel draft/i }));
    await user.click(screen.getByRole("button", { name: "Add" }));
    await user.click(screen.getByRole("menuitem", { name: /Add image/ }));

    await waitFor(() => expect(chooseVideoVisualAsset).toHaveBeenCalledOnce());
    expect(addVideoVisualAsset).not.toHaveBeenCalled();
    expect(screen.getByRole("status")).toHaveTextContent("Image selection cancelled.");
    expect(screen.queryByRole("group", { name: "Visuals track" })).not.toBeInTheDocument();
  });

  it("shows only scene and clip titles while preserving exact timing accessibly, and remembers timeline size", async () => {
    const user = userEvent.setup();
    const first = render(<VideoStudioView service={createBrowserPreviewVideoService()} />);
    await user.click(await screen.findByRole("button", { name: /Creator update · Reel draft/i }));

    const sceneButton = screen.getByRole("button", { name: /1\. Hook: where I’ve been\. Source 00:14 to 00:32/ });
    expect(sceneButton.children).toHaveLength(2);
    expect(sceneButton).toHaveTextContent("1Hook: where I’ve been");
    expect(sceneButton).not.toHaveTextContent("Source 00:14");
    const clipButton = screen.getAllByRole("button", { name: /Hook: where I’ve been, project 00:00 to 00:18/ })[0];
    expect(clipButton.children).toHaveLength(1);
    expect(clipButton).toHaveTextContent("Hook: where I’ve been");

    await user.selectOptions(screen.getByRole("combobox", { name: "Timeline size" }), "collapsed");
    expect(screen.getByRole("region", { name: "Video timeline" })).toHaveAttribute("data-timeline-mode", "collapsed");
    expect(window.localStorage.getItem("soundar.video-studio.timeline-mode")).toBe("collapsed");
    first.unmount();

    render(<VideoStudioView service={createBrowserPreviewVideoService()} />);
    await user.click(await screen.findByRole("button", { name: /Creator update · Reel draft/i }));
    expect(screen.getByRole("region", { name: "Video timeline" })).toHaveAttribute("data-timeline-mode", "collapsed");
  });

  it("persists timeline gestures and uses exact merge/split operations for undo and redo", async () => {
    const user = userEvent.setup();
    const service = createBrowserPreviewVideoService();
    const editTimeline = vi.spyOn(service, "editVideoTimeline");
    render(<VideoStudioView service={service} />);
    await user.click(await screen.findByRole("button", { name: /Creator update · Reel draft/i }));

    fireEvent.change(screen.getByRole("slider", { name: "Timeline playhead" }), { target: { value: "10000" } });
    await user.click(screen.getByRole("button", { name: "Split selected scene" }));
    await waitFor(() => expect(screen.getAllByText(/Split scene saved/i)).toHaveLength(2));
    expect(editTimeline).toHaveBeenCalledTimes(1);
    expect(editTimeline.mock.calls[0]?.[0]).toMatchObject({
      expected_revision: 0,
      base_version_id: "creator-update-v1",
      operations: [{ type: "split_scene", scene_id: "scene-clip-1", at_timeline_us: 10_000_000 }],
    });

    const undo = screen.getByRole("button", { name: "Undo last timeline edit" });
    expect(undo).toBeEnabled();
    await user.click(undo);
    await waitFor(() => expect(editTimeline).toHaveBeenCalledTimes(2));
    expect(editTimeline.mock.calls[1]?.[0].operations).toEqual([
      expect.objectContaining({ type: "merge_scenes", first_scene_id: "scene-clip-1" }),
    ]);

    const redo = screen.getByRole("button", { name: "Redo last timeline edit" });
    expect(redo).toBeEnabled();
    await user.click(redo);
    await waitFor(() => expect(editTimeline).toHaveBeenCalledTimes(3));
    expect(editTimeline.mock.calls[2]?.[0].operations).toEqual([
      { type: "split_scene", scene_id: "scene-clip-1", at_timeline_us: 10_000_000 },
    ]);
    await user.click(screen.getByRole("button", { name: "Undo last timeline edit" }));
    await waitFor(() => expect(editTimeline).toHaveBeenCalledTimes(4));
    expect(editTimeline.mock.calls[3]?.[0].operations).toEqual(editTimeline.mock.calls[1]?.[0].operations);
  });

  it("rolls back a failed timeline mutation without leaving the editor", async () => {
    const user = userEvent.setup();
    const base = createBrowserPreviewVideoService();
    const editVideoTimeline = vi.fn(async () => { throw new Error("video.revision_conflict: Project changed"); });
    render(<VideoStudioView service={{ ...base, editVideoTimeline }} />);
    await user.click(await screen.findByRole("button", { name: /Creator update · Reel draft/i }));
    fireEvent.change(screen.getByRole("slider", { name: "Timeline playhead" }), { target: { value: "10000" } });

    await user.click(screen.getByRole("button", { name: "Split selected scene" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(/timeline was restored.*revision_conflict/i);
    expect(screen.getByRole("heading", { name: "Creator update · Reel draft" })).toBeVisible();
    expect(screen.getAllByRole("button", { name: /^\d\. .*Source/ })).toHaveLength(3);
  });

  it("persists keyboard trim but reserves destructive undo for project revision history", async () => {
    const user = userEvent.setup();
    const service = createBrowserPreviewVideoService();
    const editTimeline = vi.spyOn(service, "editVideoTimeline");
    render(<VideoStudioView service={service} />);
    await user.click(await screen.findByRole("button", { name: /Creator update · Reel draft/i }));

    fireEvent.keyDown(screen.getByRole("separator", { name: "Trim start of Hook: where I’ve been" }), { key: "ArrowRight" });

    expect(await screen.findByText(/Restore destructive trims from project revision history/i)).toBeVisible();
    expect(editTimeline).toHaveBeenCalledWith(expect.objectContaining({ operations: [{ type: "trim_scene", scene_id: "scene-clip-1", source_start_us: 14_100_000, source_end_us: 32_000_000 }] }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Undo last timeline edit" })).toBeDisabled());
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
    await user.click(screen.getByRole("tab", { name: "Layout" }));
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
    const analyzedCandidates = structuredClone(draft.manifest.candidates);
    draft.status = "analyzing";
    draft.duration_ms = 0;
    draft.scene_count = 0;
    draft.manifest.candidates = [];
    draft.manifest.scenes = [];
    draft.manifest.timeline = {
      ...draft.manifest.timeline,
      duration_ms: 0,
      tracks: draft.manifest.timeline.tracks.map((track) => ({ ...track, items: [] })),
    };
    draft.workflow_job = {
      id: "analysis-job-durable-1", project_id: draft.id, phase: "analyze", status: "failed",
      progress: 0.46, title: "Analyze source", detail: "Interrupted during transcription",
      durable: true, created_at: draft.created_at, updated_at: draft.updated_at,
      error: "Application restarted",
    };
    draft.recoverable_job = draft.workflow_job;
    const completed = structuredClone(draft);
    completed.status = "review";
    completed.manifest.candidates = analyzedCandidates;
    completed.duration_ms = 0;
    completed.scene_count = 0;
    completed.manifest.scenes = [];
    completed.manifest.timeline = {
      ...completed.manifest.timeline,
      duration_ms: 0,
      tracks: completed.manifest.timeline.tracks.map((track) => ({ ...track, items: [] })),
    };
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
    draft.duration_ms = 0;
    draft.scene_count = 0;
    draft.manifest.candidates = [];
    draft.manifest.scenes = [];
    draft.manifest.timeline = {
      ...draft.manifest.timeline,
      duration_ms: 0,
      tracks: draft.manifest.timeline.tracks.map((track) => ({ ...track, items: [] })),
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
