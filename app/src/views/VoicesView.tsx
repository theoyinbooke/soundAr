import { Check, FileAudio2, LoaderCircle, Pause, Play, Plus, RefreshCw, RotateCcw, Save, Scissors, Search, Trash2, UserRound, X } from "lucide-react";
import { useDeferredValue, useEffect, useMemo, useRef, useState } from "react";
import type { BootstrapState, HistoryItem, VoiceEvaluation, VoiceProfile } from "../types";
import { CompactAudioPlayer, Dropdown, PageHeader, Panel, RowActionMenu, Segmented, StatusText } from "../components/ui";
import { VoiceProfileDialog } from "../components/VoiceProfileDialog";
import { addVoiceReference, deleteVoiceProfile, listHistory, loadGeneratedAudio, loadVoiceAudio, measureVoiceSimilarity, pickAudioFile, processVoiceReference, saveVoiceEvaluation, synthesizeSpeech, transcribeAudio, updateVoiceReferenceTranscript } from "../lib/bridge";
import { compatibleVoicesForModel, qualifiedModels } from "../lib/capabilities";

type VoiceFilter = "all" | "verified" | "draft";

function initials(name: string) {
  return name
    .split(/\s+/)
    .slice(0, 2)
    .map((part) => part[0])
    .join("")
    .toUpperCase();
}

export function VoicesView({
  bootstrap,
  voices,
  onChange,
  onGenerated,
  onUseVoice,
}: {
  bootstrap: BootstrapState;
  voices: VoiceProfile[];
  onChange: (voices: VoiceProfile[]) => void;
  onGenerated: (item: HistoryItem) => void;
  onUseVoice: (id: string) => void;
}) {
  const [query, setQuery] = useState("");
  const deferredQuery = useDeferredValue(query.trim().toLowerCase());
  const [filter, setFilter] = useState<VoiceFilter>("all");
  const [selectedId, setSelectedId] = useState(voices[0]?.id ?? "");
  const [showAdd, setShowAdd] = useState(false);
  const [formError, setFormError] = useState<string>();
  const [referenceBusy, setReferenceBusy] = useState(false);
  const [selectedReferenceId, setSelectedReferenceId] = useState("");
  const [editingReference, setEditingReference] = useState(false);
  const [trimStart, setTrimStart] = useState(0);
  const [trimEnd, setTrimEnd] = useState(0);
  const [removeSilence, setRemoveSilence] = useState(true);
  const [normalize, setNormalize] = useState(true);
  const [transcript, setTranscript] = useState("");
  const [evaluationModel, setEvaluationModel] = useState("");
  const [evaluationBusy, setEvaluationBusy] = useState(false);
  const [evaluationAudioUrl, setEvaluationAudioUrl] = useState<string>();
  const [evaluationNotes, setEvaluationNotes] = useState("");
  const [previewUrl, setPreviewUrl] = useState<string>();
  const [previewVoiceId, setPreviewVoiceId] = useState<string>();
  const [previewing, setPreviewing] = useState(false);
  const audioRef = useRef<HTMLAudioElement>(null);

  const filtered = useMemo(() => {
    return voices.filter((voice) => {
      if (filter === "verified" && voice.consent !== "confirmed") return false;
      if (filter === "draft" && voice.state !== "draft") return false;
      if (!deferredQuery) return true;
      return [voice.name, voice.style, voice.sample_label, ...voice.engines].join(" ").toLowerCase().includes(deferredQuery);
    });
  }, [deferredQuery, filter, voices]);

  const selected = voices.find((voice) => voice.id === selectedId) ?? voices[0];
  const selectedReference = selected?.references?.find((reference) => reference.id === selectedReferenceId)
    ?? selected?.references?.find((reference) => reference.active)
    ?? selected?.references?.[0];
  const cloneModels = useMemo(() => qualifiedModels(bootstrap, "tts").filter((model) => {
    const capability = bootstrap.engine_capabilities.find((entry) => entry.id === model.engine);
    return capability?.voice_modes.includes("reference") && Boolean(selected) && compatibleVoicesForModel(bootstrap, model, [selected]).length > 0;
  }), [bootstrap, selected]);
  const transcriptionModel = qualifiedModels(bootstrap, "stt")[0];
  const similarityModel = qualifiedModels(bootstrap, "speaker-verification")[0];
  const selectedEvaluation = selected?.evaluations?.find((evaluation) => evaluation.model_id === evaluationModel && evaluation.reference_id === selectedReference?.id);

  useEffect(() => {
    const reference = selected?.references?.find((item) => item.active) ?? selected?.references?.[0];
    setSelectedReferenceId(reference?.id ?? "");
    setEditingReference(false);
    setTrimStart(reference?.processing.selection_start_seconds ?? 0);
    setTrimEnd(reference?.processing.selection_end_seconds ?? reference?.analysis.duration_seconds ?? 0);
    setRemoveSilence(reference?.processing.remove_silence ?? true);
    setNormalize(reference?.processing.normalize ?? true);
    setTranscript(reference?.transcript_text ?? "");
    setEvaluationModel(cloneModels[0]?.model_id ?? "");
  }, [selectedId]);

  useEffect(() => {
    if (!selectedReference) return;
    setTrimStart(selectedReference.processing.selection_start_seconds ?? 0);
    setTrimEnd(selectedReference.processing.selection_end_seconds ?? selectedReference.analysis.duration_seconds ?? 0);
    setRemoveSilence(selectedReference.processing.remove_silence ?? true);
    setNormalize(selectedReference.processing.normalize ?? true);
    setTranscript(selectedReference.transcript_text ?? "");
  }, [selectedReferenceId]);

  useEffect(() => {
    if (!evaluationModel && cloneModels[0]) setEvaluationModel(cloneModels[0].model_id);
  }, [cloneModels, evaluationModel]);

  useEffect(() => {
    setEvaluationNotes(selectedEvaluation?.notes ?? "");
    let cancelled = false;
    if (!selectedEvaluation) {
      setEvaluationAudioUrl(undefined);
      return () => { cancelled = true; };
    }
    void listHistory().then(async (history) => {
      const item = history.find((entry) => entry.id === selectedEvaluation.history_id);
      if (!item?.audio_path || item.missing || cancelled) return;
      const url = await loadGeneratedAudio(item.audio_path);
      if (cancelled) {
        if (url.startsWith("blob:")) URL.revokeObjectURL(url);
        return;
      }
      setEvaluationAudioUrl((current) => {
        if (current?.startsWith("blob:")) URL.revokeObjectURL(current);
        return url;
      });
    }).catch((caught) => {
      if (!cancelled) setFormError(caught instanceof Error ? caught.message : String(caught));
    });
    return () => { cancelled = true; };
  }, [selectedEvaluation?.history_id]);

  useEffect(() => () => {
    if (previewUrl?.startsWith("blob:")) URL.revokeObjectURL(previewUrl);
    if (evaluationAudioUrl?.startsWith("blob:")) URL.revokeObjectURL(evaluationAudioUrl);
  }, [previewUrl, evaluationAudioUrl]);

  function replaceVoice(updated: VoiceProfile) {
    onChange(voices.map((voice) => voice.id === updated.id ? updated : voice));
  }

  async function saveTranscript() {
    if (!selected || !selectedReference) return;
    setReferenceBusy(true); setFormError(undefined);
    try { replaceVoice(await updateVoiceReferenceTranscript(selected.id, selectedReference.id, transcript, transcript.trim() ? "corrected" : "none")); }
    catch (caught) { setFormError(caught instanceof Error ? caught.message : String(caught)); }
    finally { setReferenceBusy(false); }
  }

  async function transcribeReference() {
    if (!selected || !selectedReference?.processed_path || !transcriptionModel || referenceBusy) return;
    setReferenceBusy(true); setFormError(undefined);
    try {
      const record = await transcribeAudio(transcriptionModel.model_id, selectedReference.processed_path);
      setTranscript(record.text);
      replaceVoice(await updateVoiceReferenceTranscript(selected.id, selectedReference.id, record.text, "automatic"));
    } catch (caught) { setFormError(caught instanceof Error ? caught.message : String(caught)); }
    finally { setReferenceBusy(false); }
  }

  async function applyReferenceEdit() {
    if (!selected || !selectedReference || referenceBusy) return;
    setReferenceBusy(true); setFormError(undefined);
    try {
      const updated = await processVoiceReference(selected.id, selectedReference.id, { trim_start_seconds: trimStart, trim_end_seconds: trimEnd, remove_silence: removeSilence, normalize, peak_target_dbfs: -1 });
      replaceVoice(updated); setEditingReference(false);
    } catch (caught) { setFormError(caught instanceof Error ? caught.message : String(caught)); }
    finally { setReferenceBusy(false); }
  }

  async function runEvaluation() {
    if (!selected || !selectedReference?.processed_path || !evaluationModel || evaluationBusy) return;
    const model = cloneModels.find((item) => item.model_id === evaluationModel);
    if (!model) return;
    const script = "Names, numbers, and emotion matter. soundAr should preserve clarity at 10:45, with warmth and natural pacing.";
    setEvaluationBusy(true); setFormError(undefined);
    try {
      const result = await synthesizeSpeech({ model_id: model.model_id, text: script, speaker: "default", language: model.languages[0] ?? "en", reference_audio_path: selectedReference.processed_path, speed: 1, seed: 42817, output_format: "wav", title: `${selected.name} voice evaluation`, voice_name: selected.name });
      onGenerated(result);
      const evaluation = await saveVoiceEvaluation({ id: crypto.randomUUID(), voice_id: selected.id, reference_id: selectedReference.id, model_id: model.model_id, history_id: result.id, script, decision: "pending", notes: "" });
      replaceVoice({ ...selected, evaluations: [evaluation, ...(selected.evaluations ?? []).filter((item) => item.id !== evaluation.id)] });
      if (result.audio_path) {
        if (evaluationAudioUrl?.startsWith("blob:")) URL.revokeObjectURL(evaluationAudioUrl);
        setEvaluationAudioUrl(await loadGeneratedAudio(result.audio_path));
      }
    } catch (caught) { setFormError(caught instanceof Error ? caught.message : String(caught)); }
    finally { setEvaluationBusy(false); }
  }

  async function decideEvaluation(decision: VoiceEvaluation["decision"]) {
    if (!selected || !selectedEvaluation) return;
    try {
      const updated = await saveVoiceEvaluation({ ...selectedEvaluation, decision, notes: evaluationNotes.trim() });
      replaceVoice({ ...selected, evaluations: [updated, ...(selected.evaluations ?? []).filter((item) => item.id !== updated.id)] });
    } catch (caught) { setFormError(caught instanceof Error ? caught.message : String(caught)); }
  }

  async function measureSimilarity() {
    if (!selected || !selectedEvaluation || !similarityModel || evaluationBusy) return;
    setEvaluationBusy(true); setFormError(undefined);
    try {
      const updated = await measureVoiceSimilarity(selectedEvaluation.id, similarityModel.model_id);
      replaceVoice({ ...selected, evaluations: [updated, ...(selected.evaluations ?? []).filter((item) => item.id !== updated.id)] });
    } catch (caught) { setFormError(caught instanceof Error ? caught.message : String(caught)); }
    finally { setEvaluationBusy(false); }
  }

  async function togglePreview(voice: VoiceProfile) {
    const path = voice.local_path;
    if (!path) return;
    try {
      if (audioRef.current && previewUrl && previewVoiceId === voice.id) {
        if (audioRef.current.paused) await audioRef.current.play(); else audioRef.current.pause();
        return;
      }
      audioRef.current?.pause();
      const url = await loadVoiceAudio(path);
      if (previewUrl?.startsWith("blob:")) URL.revokeObjectURL(previewUrl);
      setSelectedId(voice.id);
      setPreviewVoiceId(voice.id);
      setPreviewUrl(url);
      window.setTimeout(() => void audioRef.current?.play(), 0);
    } catch (caught) {
      setFormError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function addReference() {
    if (!selected || selected.state === "preset" || referenceBusy) return;
    const source = await pickAudioFile();
    if (!source) return;
    setReferenceBusy(true);
    setFormError(undefined);
    try {
      const updated = await addVoiceReference(selected.id, source);
      onChange(voices.map((voice) => voice.id === updated.id ? updated : voice));
      setSelectedReferenceId(updated.references?.find((reference) => reference.active)?.id ?? "");
    } catch (caught) {
      setFormError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setReferenceBusy(false);
    }
  }

  async function removeVoice(voice: VoiceProfile) {
    if (voice.state === "preset") return;
    if (!window.confirm(`Delete ${voice.name} and its managed reference audio? Existing generation records will remain.`)) return;
    try {
      if (await deleteVoiceProfile(voice.id)) {
        const remaining = voices.filter((item) => item.id !== voice.id);
        onChange(remaining);
        setSelectedId(remaining[0]?.id ?? "");
      }
    } catch (caught) {
      setFormError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  return (
    <div className="page voices-page">
      <PageHeader title="Voices" subtitle="Curate voice identities, reference samples, consent, and engine compatibility." />

      <div className="data-toolbar">
        <label className="search-control">
          <Search aria-hidden="true" size={14} />
          <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search voice profiles..." />
        </label>
        <Segmented
          label="Voice filter"
          value={filter}
          onChange={setFilter}
          options={[
            { value: "all", label: "All" },
            { value: "verified", label: "Verified" },
            { value: "draft", label: "Drafts" },
          ]}
        />
        <button className="button button-primary" type="button" onClick={() => setShowAdd(true)}>
          <Plus aria-hidden="true" size={14} />
          Add voice profile
        </button>
      </div>

      <div className="voice-layout">
        <Panel className="table-panel voice-table-panel" ariaLabel="Voice clone library">
          <div className="table-scroll">
            <table className="data-table voice-table">
              <thead>
                <tr>
                  <th>Voice</th>
                  <th>Sample</th>
                  <th>Engines</th>
                  <th>Consent</th>
                  <th>State</th>
                  <th>Analysis</th>
                  <th aria-label="Actions" />
                </tr>
              </thead>
              <tbody>
                {filtered.map((voice) => (
                  <tr className={selected?.id === voice.id ? "is-selected" : ""} key={voice.id} onClick={() => setSelectedId(voice.id)}>
                    <td>
                      <div className="voice-name-cell">
                        <span className={`voice-mark tone-${voice.color}`}>{initials(voice.name)}</span>
                        <div><strong>{voice.name}</strong><small>{voice.style}</small></div>
                      </div>
                    </td>
                    <td>{voice.sample_seconds ? `${voice.sample_seconds} sec / clean` : voice.sample_label}</td>
                    <td>{voice.engines.join(" / ")}</td>
                    <td><StatusText tone={voice.consent === "confirmed" ? "success" : voice.consent === "pending" ? "danger" : "muted"}>{voice.consent}</StatusText></td>
                    <td><StatusText tone={voice.state === "ready" ? "success" : voice.state === "draft" ? "danger" : "warning"}>{voice.state}</StatusText></td>
                    <td>
                      <StatusText tone={voice.analysis?.warnings?.length ? "warning" : voice.state === "ready" ? "success" : "muted"}>{voice.analysis?.warnings?.length ? `${voice.analysis.warnings.length} warning` : voice.state === "ready" ? "Analyzed" : "--"}</StatusText>
                    </td>
                    <td>
                      <div className="table-row-actions">
                        <button
                          className="icon-button"
                          type="button"
                          title={previewing && previewVoiceId === voice.id ? `Pause ${voice.name}` : `Play ${voice.name}`}
                          aria-label={previewing && previewVoiceId === voice.id ? `Pause ${voice.name}` : `Play ${voice.name}`}
                          disabled={!voice.local_path}
                          onClick={(event) => {
                            event.stopPropagation();
                            void togglePreview(voice);
                          }}
                        >
                          {previewing && previewVoiceId === voice.id ? <Pause aria-hidden="true" size={12} /> : <Play aria-hidden="true" size={12} />}
                        </button>
                        <RowActionMenu
                          label={`More actions for ${voice.name}`}
                          actions={[
                            {
                              label: "View details",
                              icon: <UserRound aria-hidden="true" size={12} />,
                              onSelect: () => setSelectedId(voice.id),
                            },
                            {
                              label: "Use voice",
                              icon: <Check aria-hidden="true" size={12} />,
                              disabled: voice.state !== "ready" && voice.state !== "preset",
                              onSelect: () => onUseVoice(voice.id),
                            },
                            {
                              label: "Delete profile",
                              icon: <Trash2 aria-hidden="true" size={12} />,
                              disabled: voice.state === "preset",
                              danger: true,
                              onSelect: () => removeVoice(voice),
                            },
                          ]}
                        />
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <div className="table-footnote">
            <span>{voices.length} profiles / {voices.filter((voice) => voice.state === "ready").length} clone-ready / all data local</span>
            <button className="text-button" type="button" onClick={() => setShowAdd(true)}>Import sample</button>
          </div>
        </Panel>

        {selected ? (
          <Panel className="voice-inspector" ariaLabel="Selected voice details">
            <span className="section-label">Selected voice</span>
            <h2>{selected.name}</h2>
            <StatusText tone="warning">{selected.style}</StatusText>

            <div className="sample-compact">
              <div><span className="section-label">Reference 01 / {selected.sample_seconds || "--"} sec</span><strong>{selected.sample_label}</strong></div>
              <button className="icon-button" type="button" title={previewing && previewVoiceId === selected.id ? "Pause reference" : "Play reference"} disabled={!selected.local_path} onClick={() => void togglePreview(selected)}>{previewing && previewVoiceId === selected.id ? <Pause size={12} /> : <Play size={12} />}</button>
            </div>
            <audio ref={audioRef} className="visually-hidden" preload="metadata" src={previewUrl} onPlay={() => setPreviewing(true)} onPause={() => setPreviewing(false)} onEnded={() => setPreviewing(false)} />

            <span className="section-label inspector-section">Provenance</span>
            <dl className="compact-definition-list">
              <div><dt>Source</dt><dd>{selected.state === "preset" ? "Built in" : "Owner recorded"}</dd></div>
              <div><dt>Consent</dt><dd><StatusText tone={selected.consent === "confirmed" ? "success" : "warning"}>{selected.consent}</StatusText></dd></div>
              <div><dt>Storage</dt><dd>Local only</dd></div>
              <div><dt>Profile</dt><dd>{selected.state}</dd></div>
              {selected.analysis?.sample_rate ? <div><dt>Sample rate</dt><dd>{(selected.analysis.sample_rate / 1000).toFixed(1)} kHz</dd></div> : null}
              {selected.analysis?.peak_dbfs !== undefined ? <div><dt>Peak</dt><dd>{selected.analysis.peak_dbfs.toFixed(1)} dBFS</dd></div> : null}
              {selected.analysis?.silence_ratio !== undefined ? <div><dt>Silence</dt><dd>{Math.round(selected.analysis.silence_ratio * 100)}%</dd></div> : null}
            </dl>

            <span className="section-label inspector-section">Engine coverage</span>
            <div className="engine-coverage">
              {selected.engines.map((engine) => { const accepted = selected.evaluations?.some((item) => item.decision === "accepted" && item.model_id.toLowerCase().includes(engine.toLowerCase().split(" ")[0])); return <div key={engine}><strong>{engine}</strong><StatusText tone={accepted ? "success" : selected.state === "ready" ? "warning" : "danger"}>{accepted ? "Accepted" : selected.state === "ready" ? "Audio ready" : selected.state === "preset" ? "Preset" : "Needs review"}</StatusText></div>; })}
              {!selected.engines.includes("Kokoro") ? <div><strong>Kokoro</strong><StatusText tone="warning">Preset only</StatusText></div> : null}
            </div>

            {selected.state !== "preset" ? <><span className="section-label inspector-section">Managed references</span><div className="voice-reference-list">{(selected.references ?? []).map((reference, index) => <button className={reference.id === selectedReference?.id ? "is-selected" : ""} key={reference.id} type="button" onClick={() => setSelectedReferenceId(reference.id)}><div><strong>Reference {String(index + 1).padStart(2, "0")}</strong><small>{reference.processed_path ? `${reference.analysis.duration_seconds?.toFixed(1) ?? "--"} sec / ${reference.revision_count ?? 0} revisions` : reference.analysis.processing_error ? "Processing failed" : "Processing"}</small></div><StatusText tone={reference.active ? "success" : reference.processed_path ? "warning" : "danger"}>{reference.active ? "Active" : reference.processed_path ? "Review" : "Failed"}</StatusText></button>)}</div><button className="button button-secondary add-reference-button" type="button" disabled={referenceBusy} onClick={() => void addReference()}>{referenceBusy ? <LoaderCircle className="spin" size={13} /> : <Plus size={13} />}{referenceBusy ? "Processing reference" : "Add reference"}</button></> : null}

            {selected.state !== "preset" && selectedReference ? <div className="voice-lab-tools"><div className="voice-lab-heading"><span className="section-label">Reference editor</span><button className="text-button" type="button" onClick={() => setEditingReference((value) => !value)}>{editingReference ? "Close" : "Edit"}</button></div>{editingReference ? <div className="reference-editor"><div className="reference-waveform" aria-label="Reference waveform">{(selectedReference.analysis.waveform ?? []).slice(0, 72).map((peak, index) => <i key={index} style={{ height: `${Math.max(3, peak * 28)}px` }} />)}</div><div className="trim-fields"><label className="form-field"><span>Start</span><input type="number" min="0" max={trimEnd} step="0.1" value={trimStart} onChange={(event) => setTrimStart(Number(event.target.value))} /></label><label className="form-field"><span>End</span><input type="number" min={trimStart} max={selectedReference.analysis.duration_seconds} step="0.1" value={trimEnd} onChange={(event) => setTrimEnd(Number(event.target.value))} /></label></div><label className="toggle-row voice-lab-toggle"><span><strong>Remove edge silence</strong></span><input type="checkbox" checked={removeSilence} onChange={(event) => setRemoveSilence(event.target.checked)} /></label><label className="toggle-row voice-lab-toggle"><span><strong>Peak normalize</strong></span><input type="checkbox" checked={normalize} onChange={(event) => setNormalize(event.target.checked)} /></label><button className="button button-primary" type="button" title={bootstrap.runtime === "browser" ? "Reference processing requires the desktop runtime" : undefined} disabled={bootstrap.runtime === "browser" || referenceBusy || trimEnd <= trimStart} onClick={() => void applyReferenceEdit()}><Scissors size={13} />Apply as new revision</button></div> : null}<label className="form-field reference-transcript"><span>Reference transcript</span><textarea value={transcript} onChange={(event) => setTranscript(event.target.value)} placeholder="Correct what the speaker says in this reference..." /></label><div className="reference-transcript-actions"><button className="button button-secondary" type="button" disabled={referenceBusy || !transcriptionModel || !selectedReference.processed_path} onClick={() => void transcribeReference()}>{referenceBusy ? <LoaderCircle className="spin" size={13} /> : <FileAudio2 size={13} />}Transcribe</button><button className="button button-secondary" type="button" disabled={referenceBusy || transcript === (selectedReference.transcript_text ?? "")} onClick={() => void saveTranscript()}><Save size={13} />Save correction</button></div></div> : null}

            {selected.state !== "preset" && selectedReference?.active ? <div className="voice-evaluation"><div className="voice-lab-heading"><span className="section-label">Engine evaluation</span>{selectedEvaluation ? <StatusText tone={selectedEvaluation.decision === "accepted" ? "success" : selectedEvaluation.decision === "rejected" ? "danger" : "warning"}>{selectedEvaluation.decision}</StatusText> : null}</div><Dropdown ariaLabel="Evaluation model" value={evaluationModel} onChange={setEvaluationModel} options={cloneModels.map((model) => ({ value: model.model_id, label: model.model_id }))} /><CompactAudioPlayer src={evaluationAudioUrl} label="voice evaluation" /><button className="button button-secondary" type="button" disabled={!evaluationModel || evaluationBusy || !cloneModels.length} onClick={() => void runEvaluation()}>{evaluationBusy ? <LoaderCircle className="spin" size={13} /> : <RotateCcw size={13} />}{evaluationBusy ? "Working" : selectedEvaluation ? "Regenerate evaluation" : "Generate evaluation"}</button>{selectedEvaluation ? <><div className="similarity-row"><div><span className="section-label">Speaker similarity</span><strong>{selectedEvaluation.speaker_similarity === null || selectedEvaluation.speaker_similarity === undefined ? "Not measured" : selectedEvaluation.speaker_similarity.toFixed(3)}</strong><small>{selectedEvaluation.similarity_model_id?.split("/").at(-1) ?? "Cosine x-vector evidence"}</small></div><button className="button button-secondary" type="button" disabled={!similarityModel || evaluationBusy} title={!similarityModel ? "Install the WavLM speaker-verification model" : "Compare normalized speaker embeddings"} onClick={() => void measureSimilarity()}>{evaluationBusy ? <LoaderCircle className="spin" size={13} /> : <RefreshCw size={13} />}{selectedEvaluation.speaker_similarity === null || selectedEvaluation.speaker_similarity === undefined ? "Measure" : "Remeasure"}</button></div>{!similarityModel ? <StatusText tone="warning">Install WavLM Speaker Verification in Models to measure likeness.</StatusText> : <small className="similarity-note">Comparative score only. Thresholds depend on the voices and recording conditions.</small>}<label className="form-field evaluation-notes"><span>Review notes</span><textarea value={evaluationNotes} onChange={(event) => setEvaluationNotes(event.target.value)} placeholder="Clarity, likeness, pacing, or pronunciation notes..." /></label><div className="evaluation-actions"><button className="button button-secondary danger-button" type="button" onClick={() => void decideEvaluation("rejected")}><X size={13} />Reject</button><button className="button button-primary" type="button" onClick={() => void decideEvaluation("accepted")}><Check size={13} />Accept</button></div></> : null}{!cloneModels.length ? <StatusText tone="warning">Install a qualified clone-capable model to evaluate this reference.</StatusText> : null}</div> : null}

            <div className="inspector-bottom-actions">
              <button className="button button-secondary danger-button" type="button" disabled={selected.state === "preset"} onClick={() => void removeVoice(selected)}><Trash2 size={13} />Delete</button>
              <button className="button button-primary" type="button" disabled={selected.state !== "ready" && selected.state !== "preset"} onClick={() => onUseVoice(selected.id)}>Use voice</button>
            </div>
            {formError ? <StatusText tone="danger">{formError}</StatusText> : null}
          </Panel>
        ) : null}
      </div>

      {showAdd ? <VoiceProfileDialog onClose={() => setShowAdd(false)} onCreated={(voice) => { onChange([...voices, voice]); setSelectedId(voice.id); setShowAdd(false); }} /> : null}
    </div>
  );
}
