import { Gauge, Play, RotateCcw } from "lucide-react";
import { useMemo, useState } from "react";
import type { BootstrapState, HistoryItem, MeasuredBenchmarkRun } from "../types";
import { isDesktopRuntime, prepareBenchmarkEngine, releaseBenchmarkEngine, saveBenchmarkRun, synthesizeSpeech, transcribeAudio } from "../lib/bridge";
import { canSynthesizeWithoutReference, qualifiedModels } from "../lib/capabilities";
import { EmptyState, MetricStrip, PageHeader, Panel, SelectField, StatusText } from "../components/ui";

const BENCHMARK_TEXT = "soundAr measures this engine on the current machine using real generated audio.";

export function BenchmarksView({
  bootstrap,
  onGenerated,
}: {
  bootstrap: BootstrapState;
  onGenerated?: (item: HistoryItem) => void;
}) {
  const models = useMemo(() => qualifiedModels(bootstrap, "tts").filter((model) => canSynthesizeWithoutReference(bootstrap, model)), [bootstrap]);
  const [modelId, setModelId] = useState(models.find((model) => model.engine === "kokoro")?.model_id ?? models[0]?.model_id ?? "");
  const [runs, setRuns] = useState<MeasuredBenchmarkRun[]>(bootstrap.benchmarks);
  const [running, setRunning] = useState(false);
  const [error, setError] = useState<string>();
  const selected = models.find((model) => model.model_id === modelId);
  const verifier = qualifiedModels(bootstrap, "stt").find((model) => model.model_id === "openai/whisper-tiny") ?? qualifiedModels(bootstrap, "stt")[0];
  const canMeasure = isDesktopRuntime() && Boolean(selected && verifier);
  const successful = runs.filter((run) => run.model_id === modelId);
  const bestRtf = successful.length ? Math.min(...successful.map((run) => run.rtf)) : undefined;
  const averageRtf = successful.length ? successful.reduce((sum, run) => sum + run.rtf, 0) / successful.length : undefined;
  const intelligibilityRuns = successful.filter((run) => run.word_error_rate !== undefined);
  const meanWer = intelligibilityRuns.length ? intelligibilityRuns.reduce((sum, run) => sum + (run.word_error_rate ?? 0), 0) / intelligibilityRuns.length : undefined;
  const characterRuns = successful.filter((run) => run.character_error_rate !== undefined);
  const meanCer = characterRuns.length ? characterRuns.reduce((sum, run) => sum + (run.character_error_rate ?? 0), 0) / characterRuns.length : undefined;

  async function runSuite() {
    if (running || !selected) return;
    setRunning(true);
    setError(undefined);
    let benchmarkToken: string | undefined;
    try {
      benchmarkToken = (await prepareBenchmarkEngine(selected.model_id)).token;
      const generated: HistoryItem[] = [];
      const measured: MeasuredBenchmarkRun[] = [];
      for (let iteration = 0; iteration < 3; iteration += 1) {
        const result = await synthesizeSpeech({
          model_id: selected.model_id,
          text: BENCHMARK_TEXT,
          speaker: selected.engine === "kokoro" ? "af_heart" : "default",
          language: "en",
          speed: 1,
          seed: 42817,
          output_format: "wav",
          title: `Benchmark ${selected.model_id.split("/").at(-1)} ${iteration + 1}`,
          voice_name: "Benchmark voice",
          benchmark_token: benchmarkToken,
        });
        onGenerated?.(result);
        generated.push(result);
      }
      for (const result of generated) {
        if (!verifier || !result.audio_path) throw new Error("Install a speech-to-text verifier before running intelligibility measurements.");
        const transcription = await transcribeAudio(verifier.model_id, result.audio_path);
        const install = bootstrap.installed.find((model) => model.model_id === result.model_id);
        measured.push(await saveBenchmarkRun({
          history_id: result.id,
          transcription_id: transcription.id,
          model_revision: install?.revision,
          gpu_name: bootstrap.system.gpu_name,
          driver_version: bootstrap.system.driver_version,
          app_version: __APP_VERSION__,
        }));
      }
      setRuns((current) => [...measured, ...current]);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      if (benchmarkToken) await releaseBenchmarkEngine(benchmarkToken).catch(() => false);
      setRunning(false);
    }
  }

  return (
    <div className="page benchmarks-page">
      <PageHeader
        title="Benchmarks"
        subtitle="Run repeatable synthesis measurements on this machine. Listening quality remains a human decision."
        actions={
          <button className="button button-primary" type="button" onClick={() => void runSuite()} disabled={running || !canMeasure} title={!isDesktopRuntime() ? "Measured suites require the desktop runtime" : !verifier ? "Install a speech-to-text verifier" : undefined}>
            {running ? <RotateCcw className="spin" aria-hidden="true" size={14} /> : <Play aria-hidden="true" size={14} />}
            {running ? "Running 3 passes" : "Run measured suite"}
          </button>
        }
      />

      <MetricStrip metrics={[
        { value: bestRtf === undefined ? "--" : `${bestRtf.toFixed(3)}x`, label: "Best RTF", tone: "success" },
        { value: averageRtf === undefined ? "--" : `${averageRtf.toFixed(3)}x`, label: "Mean RTF", tone: "success" },
        { value: meanWer === undefined ? "--" : `${(meanWer * 100).toFixed(1)}%`, label: "Mean WER" },
        { value: meanCer === undefined ? "--" : `${(meanCer * 100).toFixed(1)}%`, label: "Mean CER" },
      ]} />

      <div className="data-toolbar benchmark-toolbar">
        <div className="toolbar-note"><Gauge aria-hidden="true" size={14} /><span>{bootstrap.system.gpu_name} / cold, warm, warm / fixed text and seed</span></div>
        <div className="benchmark-model-select"><SelectField label="Engine under test" value={modelId} onChange={(value) => { setModelId(value); setError(undefined); }} options={models.map((model) => ({ value: model.model_id, label: model.model_id }))} /></div>
      </div>

      {error ? <div className="model-notice is-danger"><StatusText tone="danger">{error}</StatusText></div> : null}
      <Panel className="table-panel benchmark-table-panel" ariaLabel="Measured benchmark runs">
        {successful.length ? <div className="table-scroll"><table className="data-table benchmark-table">
          <thead><tr><th>Run</th><th>Engine</th><th>State</th><th>RTF</th><th>Total / startup</th><th>WER / CER</th><th>Peak VRAM</th><th>Recorded</th></tr></thead>
          <tbody>{successful.map((run, index) => <tr key={run.id}>
            <td><strong>Pass {successful.length - index}</strong><small>{run.model_id.split("/").at(-1)}</small></td>
            <td>{run.engine}<small>{run.verifier_model_id ? `checked by ${run.verifier_model_id.split("/").at(-1)}` : "legacy timing run"}</small></td>
            <td><StatusText tone={run.warm_state === "cold" ? "warning" : "success"}>{run.warm_state ?? "warm"}</StatusText></td>
            <td className="mono-cell"><strong>{run.rtf.toFixed(3)}x</strong></td>
            <td className="mono-cell">{run.end_to_end_seconds === undefined ? "--" : `${run.end_to_end_seconds.toFixed(2)}s / ${(run.runtime_overhead_seconds ?? 0).toFixed(2)}s`}</td>
            <td className="mono-cell">{run.word_error_rate === undefined ? "--" : `${(run.word_error_rate * 100).toFixed(1)}% / ${run.character_error_rate === undefined ? "--" : `${(run.character_error_rate * 100).toFixed(1)}%`}`}</td>
            <td className="mono-cell">{run.vram_mb.toFixed(0)} MB</td>
            <td className="muted-cell">{new Date(run.created_at).toLocaleTimeString()}</td>
          </tr>)}</tbody>
        </table></div> : <EmptyState title="No measured runs" detail="Choose an installed model and run the three-pass local suite." />}
        <div className="table-footnote"><span>Lower is better. The native runtime verifies cold/warm state; WER and CER use the exact generated artifact.</span><StatusText tone="warning">Experimental</StatusText></div>
      </Panel>
    </div>
  );
}
