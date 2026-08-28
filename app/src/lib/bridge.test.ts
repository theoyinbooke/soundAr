import { afterEach, describe, expect, it, vi } from "vitest";
import { createBatchRun, generateMusic, getHistoryRequest, listHistory, pauseBatchRun, resumeBatchRun, saveApplicationSetting, synthesizeSpeech, updateBatchItem, updateHistoryMetadata, verifyModel } from "./bridge";

describe("browser preview bridge", () => {
  afterEach(() => vi.useRealTimers());

  it("never fabricates durable history records", async () => {
    expect(await listHistory()).toEqual([]);
  });

  it("marks preview synthesis explicitly", async () => {
    vi.useFakeTimers();
    const generation = synthesizeSpeech({
      model_id: "hexgrad/Kokoro-82M",
      text: "Preview only",
      speaker: "af_heart",
      language: "en",
      speed: 1,
      seed: 1,
      output_format: "wav",
    });
    await vi.runAllTimersAsync();
    const result = await generation;
    expect(result.preview).toBe(true);
    expect(result.audio_path).toBeNull();
    expect(await listHistory("Preview only", { artifact_state: "unavailable" })).toEqual([result]);
    expect(await listHistory("Preview only", { artifact_state: "available" })).toEqual([]);
    expect((await updateHistoryMetadata(result.id, { favorite: true })).favorite).toBe(true);
    expect(await listHistory("Preview only", { favorite: true })).toHaveLength(1);
    const storedRequest = await getHistoryRequest(result.id);
    expect("text" in storedRequest && storedRequest.text).toBe("Preview only");
  });

  it("keeps browser music previews explicitly non-rendered and distinct from speech", async () => {
    vi.useFakeTimers();
    const generation = generateMusic({
      model_id: "facebook/musicgen-small",
      prompt: "Warm ambient synth pads",
      duration_seconds: 10,
      guidance_scale: 3,
      temperature: 1,
      top_k: 250,
      top_p: 0,
      seed: 9,
      output_format: "wav",
    });
    await vi.runAllTimersAsync();
    const result = await generation;
    expect(result.preview).toBe(true);
    expect(result.audio_path).toBeNull();
    expect(result.generation_kind).toBe("music");
    expect(result.voice).toBe("Not applicable");
    const request = await getHistoryRequest(result.id);
    expect("prompt" in request && request.prompt).toBe("Warm ambient synth pads");
  });

  it("preserves separate ACE-Step direction and lyric conditions in a non-rendered preview", async () => {
    vi.useFakeTimers();
    const generation = generateMusic({
      model_id: "ACE-Step/acestep-v15-xl-turbo-diffusers",
      prompt: "Warm indie-pop, brushed drums, close-mic lead vocal",
      lyrics: "[Verse]\nHold the light until morning comes",
      vocal_language: "en",
      duration_seconds: 20,
      inference_steps: 8,
      shift: 3,
      bpm: 96,
      seed: 42,
      output_format: "wav",
    });
    await vi.runAllTimersAsync();
    const result = await generation;

    expect(result.preview).toBe(true);
    expect(result.audio_path).toBeNull();
    expect(result.engine).toBe("acestep");
    expect(result.sample_rate).toBe(48_000);
    const request = await getHistoryRequest(result.id);
    expect("prompt" in request && request.prompt).toBe("Warm indie-pop, brushed drums, close-mic lead vocal");
    expect("lyrics" in request && request.lyrics).toBe("[Verse]\nHold the light until morning comes");
    expect("vocal_language" in request && request.vocal_language).toBe("en");
  });

  it("reports preview model integrity without pretending to inspect native files", async () => {
    const integrity = await verifyModel("hexgrad/Kokoro-82M");
    expect(integrity.state).toBe("ready");
    expect(integrity.manifest_verified).toBe(false);
  });

  it("keeps preview preferences typed without using desktop persistence", async () => {
    expect((await saveApplicationSetting("theme", "light")).theme).toBe("light");
    expect((await saveApplicationSetting("dense_tables", false)).dense_tables).toBe(false);
  });

  it("keeps successful rows while pause, resume, and failed-row retry remain distinct", async () => {
    const batch = await createBatchRun("Preview controls", ["done", "failed", "queued"], {});
    await updateBatchItem(batch.id, 0, "completed", "history-1");
    await updateBatchItem(batch.id, 1, "failed", undefined, "engine failed");
    expect((await pauseBatchRun(batch.id)).status).toBe("paused");

    const resumed = await resumeBatchRun(batch.id, 2, false);
    expect(resumed.items.map((item) => item.status)).toEqual(["completed", "failed", "queued"]);
    expect(resumed.items[0].history_id).toBe("history-1");

    const retried = await resumeBatchRun(batch.id, 2, true);
    expect(retried.items.map((item) => item.status)).toEqual(["completed", "queued", "queued"]);
    expect(retried.failed_items).toBe(0);
  });
});
