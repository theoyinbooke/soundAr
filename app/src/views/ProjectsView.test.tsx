import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { fallbackBootstrap } from "../data";
import type { BatchRunRecord, ProjectRecord, ProjectRenderBatch } from "../types";
import { ProjectsView, reconcileProjectBatchChapters } from "./ProjectsView";

const bridge = vi.hoisted(() => ({
  cancelBatchRun: vi.fn(),
  deleteProject: vi.fn(),
  exportProjectMaster: vi.fn(),
  getBatchRun: vi.fn(),
  importProjectScript: vi.fn(),
  listHistory: vi.fn(),
  loadGeneratedAudio: vi.fn(),
  pauseBatchRun: vi.fn(),
  pickProjectScript: vi.fn(),
  queueBatchRun: vi.fn(),
  resumeBatchRun: vi.fn(),
  saveProject: vi.fn(),
  synthesizeSpeech: vi.fn(),
}));

vi.mock("../lib/bridge", () => bridge);

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

const project: ProjectRecord = {
  id: "project-1",
  name: "Release narration",
  document: {
    script: "First chapter\n\nSecond chapter",
    speaker_assignments: {},
    chapters: [
      { id: "chapter-1", title: "Opening", text: "First chapter", language: "en" },
      { id: "chapter-2", title: "Closing", text: "Second chapter", language: "en" },
    ],
  },
  created_at: "2026-08-13T12:00:00Z",
  updated_at: "2026-08-13T12:00:00Z",
};

function batch(status: BatchRunRecord["status"] = "queued"): BatchRunRecord {
  return {
    id: "batch-1",
    name: "Release narration / stale chapters",
    status,
    total_items: 2,
    completed_items: status === "completed" ? 2 : 0,
    failed_items: 0,
    request: {},
    items: project.document.chapters.map((chapter, item_index) => ({
      id: `item-${item_index}`,
      item_index,
      text: chapter.text,
      status: status === "completed" ? "completed" : "queued",
      history_id: status === "completed" ? `history-${item_index}` : undefined,
      created_at: "2026-08-13T12:00:00Z",
      updated_at: "2026-08-13T12:00:00Z",
    })),
    created_at: "2026-08-13T12:00:00Z",
    updated_at: "2026-08-13T12:00:00Z",
  };
}

describe("Projects batch rendering", () => {
  it("never links an older batch result to a chapter edited after submission", () => {
    const linkage: ProjectRenderBatch = {
      batch_id: "batch-1",
      started_at: "2026-08-13T12:00:00Z",
      rows: [
        { chapter_id: "chapter-1", item_index: 0, source_text: "First chapter" },
        { chapter_id: "chapter-2", item_index: 1, source_text: "Second chapter" },
      ],
    };
    const edited = [{ ...project.document.chapters[0], text: "First chapter, revised" }, project.document.chapters[1]];
    const reconciled = reconcileProjectBatchChapters(edited, linkage, batch("completed"));
    expect(reconciled[0].history_id).toBeUndefined();
    expect(reconciled[1].history_id).toBe("history-1");
  });

  it("does not link a result after its chapter model, voice, or language changes", () => {
    const source = project.document.chapters[0];
    const linkage: ProjectRenderBatch = {
      batch_id: "batch-1",
      started_at: "2026-08-13T12:00:00Z",
      rows: [{
        chapter_id: source.id,
        item_index: 0,
        source_text: source.text,
        source_model_id: "hexgrad/Kokoro-82M",
        source_voice_id: "af_heart",
        source_language: "en",
      }],
    };
    const completed = batch("completed");
    const matching = [{ ...source, model_id: "hexgrad/Kokoro-82M", voice_id: "af_heart", language: "en" }];
    expect(reconcileProjectBatchChapters(matching, linkage, completed)[0].history_id).toBe("history-0");
    expect(reconcileProjectBatchChapters([{ ...matching[0], model_id: "microsoft/speecht5_tts" }], linkage, completed)[0].history_id).toBeUndefined();
    expect(reconcileProjectBatchChapters([{ ...matching[0], voice_id: "af_bella" }], linkage, completed)[0].history_id).toBeUndefined();
    expect(reconcileProjectBatchChapters([{ ...matching[0], language: "fr" }], linkage, completed)[0].history_id).toBeUndefined();
  });

  it("queues every stale chapter with its own model settings and persists linkage", async () => {
    const user = userEvent.setup();
    const nativeBootstrap = { ...fallbackBootstrap, runtime: "tauri" as const };
    bridge.saveProject.mockImplementation(async (value: ProjectRecord) => ({ ...project, ...value }));
    bridge.queueBatchRun.mockResolvedValue(batch());
    bridge.getBatchRun.mockResolvedValue(batch("completed"));
    bridge.listHistory.mockResolvedValue([]);

    render(<ProjectsView bootstrap={nativeBootstrap} projects={[project]} voices={nativeBootstrap.voices} onChange={vi.fn()} onGenerated={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Render stale (2)" }));

    await waitFor(() => expect(bridge.queueBatchRun).toHaveBeenCalledTimes(1));
    const [, rows, , parallelism] = bridge.queueBatchRun.mock.calls[0];
    expect(rows).toHaveLength(2);
    expect(rows[0]).toMatchObject({ text: "First chapter", name: "Release narration: Opening" });
    expect(rows[0].settings).toMatchObject({ model_id: "hexgrad/Kokoro-82M", input_mode: "text", language: "en" });
    expect(parallelism).toBe(2);
    await waitFor(() => expect(bridge.saveProject.mock.calls.some(([value]) => value.document.render_batch?.batch_id === "batch-1")).toBe(true));
    await waitFor(() => expect(bridge.saveProject.mock.calls.some(([value]) => value.document.chapters.every((chapter: { history_id?: string }) => chapter.history_id))).toBe(true));
  });

  it("restarts polling after retrying a failed project batch", async () => {
    const user = userEvent.setup();
    const nativeBootstrap = { ...fallbackBootstrap, runtime: "tauri" as const };
    const failedBatch = { ...batch("failed"), failed_items: 2, items: batch().items.map((item) => ({ ...item, status: "failed" as const, error: "Worker stopped" })) };
    const queuedBatch = { ...failedBatch, status: "queued" as const, failed_items: 0, items: failedBatch.items.map((item) => ({ ...item, status: "queued" as const, error: undefined })) };
    const linkedProject: ProjectRecord = {
      ...project,
      document: {
        ...project.document,
        render_batch: {
          batch_id: "batch-1",
          started_at: "2026-08-13T12:00:00Z",
          rows: project.document.chapters.map((chapter, item_index) => ({ chapter_id: chapter.id, item_index, source_text: chapter.text })),
        },
      },
    };
    bridge.getBatchRun.mockResolvedValueOnce(failedBatch).mockResolvedValue(queuedBatch);
    bridge.resumeBatchRun.mockResolvedValue(queuedBatch);
    bridge.saveProject.mockImplementation(async (value: ProjectRecord) => ({ ...linkedProject, ...value }));
    bridge.listHistory.mockResolvedValue([]);

    render(<ProjectsView bootstrap={nativeBootstrap} projects={[linkedProject]} voices={nativeBootstrap.voices} onChange={vi.fn()} onGenerated={vi.fn()} />);
    await user.click(await screen.findByRole("button", { name: "Retry failed" }));

    await waitFor(() => expect(bridge.resumeBatchRun).toHaveBeenCalledWith("batch-1", 2, true));
    await waitFor(() => expect(bridge.getBatchRun.mock.calls.length).toBeGreaterThanOrEqual(2));
  });
});
