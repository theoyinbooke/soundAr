import type { BootstrapState, EngineCapability, InstalledModel, RouteIntent, VoiceProfile } from "../types";

export function capabilityForModel(
  bootstrap: BootstrapState,
  model: InstalledModel | undefined,
): EngineCapability | undefined {
  return bootstrap.engine_capabilities.find((capability) => capability.id === model?.engine);
}

export function qualifiedModels(
  bootstrap: BootstrapState,
  task: "tts" | "stt" | "speaker-verification" | "alignment",
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
    if (intent === "expressive") return Boolean(capability.controls.exaggeration || capability.controls.cfg_weight);
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

  const preferredEngine = intent === "fast" ? "kokoro" : intent === "expressive" ? "chatterbox" : intent === "multilingual" ? "coqui" : undefined;
  const model = candidates.find((candidate) => candidate.engine === preferredEngine) ?? candidates[0];
  return { model, reason: `${intent[0].toUpperCase()}${intent.slice(1)} route selected from declared capabilities; run Benchmarks to replace this fallback with local evidence.`, measured: false };
}
