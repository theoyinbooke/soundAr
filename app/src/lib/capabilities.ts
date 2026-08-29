import type { BootstrapState, EngineCapability, InstalledModel, RouteIntent, VoiceProfile } from "../types";

// Breeze TTS 2 is the most expressive local voice model installed, so it is the default choice
// wherever soundAr creates speech without the user naming a model. The fallbacks matter: Breeze
// needs CUDA and ~7.9 GB of VRAM, so a machine without it must still land on a working engine.
export const DEFAULT_TTS_ENGINE = "breeze";
const DEFAULT_TTS_ENGINE_FALLBACKS = ["kokoro", "chatterbox", "fish-speech", "speecht5"];

export function defaultTtsModel(bootstrap: BootstrapState): InstalledModel | undefined {
  const models = qualifiedModels(bootstrap, "tts");
  for (const engine of [DEFAULT_TTS_ENGINE, ...DEFAULT_TTS_ENGINE_FALLBACKS]) {
    const match = models.find((model) => model.engine === engine);
    if (match) return match;
  }
  return models[0];
}

export function capabilityForModel(
  bootstrap: BootstrapState,
  model: Pick<InstalledModel, "engine"> | undefined,
): EngineCapability | undefined {
  return bootstrap.engine_capabilities.find((capability) => capability.id === model?.engine);
}

export function qualifiedModels(
  bootstrap: BootstrapState,
  task: "tts" | "stt" | "speaker-verification" | "alignment" | "music",
): InstalledModel[] {
  return bootstrap.installed.filter((model) => {
    if (model.task !== task) return false;
    if (model.integrity?.state === "repair-needed") return false;
    return capabilityForModel(bootstrap, model)?.tasks.includes(task) === true;
  });
}

export function canSynthesizeWithoutReference(
  bootstrap: BootstrapState,
  model: InstalledModel,
): boolean {
  const modes = capabilityForModel(bootstrap, model)?.voice_modes ?? [];
  return modes.includes("preset") || modes.includes("default");
}

export function compatibleVoicesForModel(
  bootstrap: BootstrapState,
  model: InstalledModel | undefined,
  voices: VoiceProfile[],
): VoiceProfile[] {
  const capability = capabilityForModel(bootstrap, model);
  if (!model || !capability) return [];
  if (capability.voice_modes.includes("preset")) {
    return voices.filter((voice) => voice.state === "preset" && voice.engines.some((engine) => engine.toLowerCase() === capability.display_name.toLowerCase()));
  }
  if (!capability.voice_modes.includes("reference")) return [];
  return voices.filter((voice) => (
    voice.state === "ready"
    && voice.consent === "confirmed"
    && Boolean(voice.local_path)
    && voice.engines.some((engine) => {
      const normalized = engine.toLowerCase();
      return normalized === capability.display_name.toLowerCase()
        || (model.engine === "chatterbox-turbo" && normalized === "chatterbox")
        || (model.engine === "coqui" && normalized === "xtts");
    })
  ));
}

export function recommendModel(
  bootstrap: BootstrapState,
  intent: Exclude<RouteIntent, "manual">,
  voices: VoiceProfile[],
): { model?: InstalledModel; reason: string; measured: boolean } {
  const candidates = qualifiedModels(bootstrap, "tts").filter((model) => {
    const capability = capabilityForModel(bootstrap, model);
    if (!capability) return false;
    if (intent === "expressive") return Boolean(capability.controls.exaggeration || capability.controls.cfg_weight || capability.controls.cfg_scale);
    if (intent === "clone") return capability.voice_modes.includes("reference") && compatibleVoicesForModel(bootstrap, model, voices).length > 0;
    if (intent === "multilingual") return capability.languages.length > 2;
    return true;
  });
  if (!candidates.length) return { reason: `No installed ${intent} route currently satisfies its capability requirements.`, measured: false };

  const medians = new Map<string, number>();
  for (const model of candidates) {
    const values = bootstrap.benchmarks.filter((run) => run.model_id === model.model_id && Number.isFinite(run.rtf)).map((run) => run.rtf).sort((a, b) => a - b);
    if (values.length) medians.set(model.model_id, values[Math.floor(values.length / 2)]);
  }
  const measured = candidates.filter((model) => medians.has(model.model_id)).sort((a, b) => (medians.get(a.model_id) ?? Infinity) - (medians.get(b.model_id) ?? Infinity));
  if (measured[0]) {
    const rtf = medians.get(measured[0].model_id) ?? 0;
    return { model: measured[0], reason: `${intent[0].toUpperCase()}${intent.slice(1)} route selected from this machine's benchmark median (${rtf.toFixed(3)}x RTF).`, measured: true };
  }

  // "Fast" stays with the small model on purpose — Breeze is 3.47B and is not the quick route.
  const preferredEngine = intent === "fast" ? "kokoro" : intent === "expressive" ? DEFAULT_TTS_ENGINE : intent === "multilingual" ? "coqui" : undefined;
  const model = candidates.find((candidate) => candidate.engine === preferredEngine) ?? candidates[0];
  return { model, reason: `${intent[0].toUpperCase()}${intent.slice(1)} route selected from declared capabilities; run Benchmarks to replace this fallback with local evidence.`, measured: false };
}
