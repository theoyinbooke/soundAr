import { LoaderCircle, Play, RotateCcw, Save, TextCursorInput, Upload, UsersRound } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import type { BootstrapState, TranscriptionRecord } from "../types";
import {
  loadTranscriptionAudio,
  alignTranscription,
  diarizeTranscription,
  pickAudioFile,
  transcribeAudio,
  updateTranscription,
  updateTranscriptionSpeakerLabels,
} from "../lib/bridge";
import { qualifiedModels } from "../lib/capabilities";
import {
  EmptyState,
  MetricStrip,
  PageHeader,
  Panel,
  SelectField,
  StatusText,
} from "../components/ui";

function timecode(seconds: number) {
  const minutes = Math.floor(seconds / 60);
  return `${minutes}:${(seconds % 60).toFixed(1).padStart(4, "0")}`;
}

function evidenceLabel(source: string | undefined) {
  return source && source !== "unavailable"
    ? source.replaceAll("-", " ")
    : "Not reported";
}

function sourceName(path: string) {
  return path.startsWith("data:") ? "Preview transcript.wav" : path.split("/").at(-1) || "Local audio";
}

export function TranscribeView({
  bootstrap,
  records,
  onChange,
}: {
  bootstrap: BootstrapState;
  records: TranscriptionRecord[];
  onChange: (records: TranscriptionRecord[]) => void;
}) {
  const models = useMemo(() => qualifiedModels(bootstrap, "stt"), [bootstrap]);
  const diarizationModels = useMemo(
    () => qualifiedModels(bootstrap, "speaker-verification"),
    [bootstrap],
  );
  const alignmentModels = useMemo(
    () => qualifiedModels(bootstrap, "alignment"),
    [bootstrap],
  );
  const [modelId, setModelId] = useState(
    models.find((model) => model.model_id === "openai/whisper-tiny")
      ?.model_id ??
      models[0]?.model_id ??
      "",
  );
  const [audioPath, setAudioPath] = useState("");
  const [active, setActive] = useState<TranscriptionRecord | undefined>(
    records[0],
  );
  const [audioUrl, setAudioUrl] = useState<string>();
  const [running, setRunning] = useState(false);
  const [cleanup, setCleanup] = useState(false);
  const [draftText, setDraftText] = useState(active?.text ?? "");
  const [draftSegments, setDraftSegments] = useState(active?.segments ?? []);
  const [saving, setSaving] = useState(false);
  const [savedState, setSavedState] = useState<string>();
  const [speakerCount, setSpeakerCount] = useState("auto");
  const [diarizing, setDiarizing] = useState(false);
  const [aligning, setAligning] = useState(false);
  const [savingLabels, setSavingLabels] = useState(false);
  const [speakerLabels, setSpeakerLabels] = useState<Record<string, string>>(
    active?.diarization?.labels ?? {},
  );
  const [error, setError] = useState<string>();
  const audioRef = useRef<HTMLAudioElement>(null);
  const playbackEndRef = useRef<number | undefined>(undefined);
  const measuredWordConfidence =
    active?.words?.filter((word) => word.confidence != null).length ?? 0;
  const correctionDirty = Boolean(
    active &&
      (draftText !== active.text ||
        JSON.stringify(draftSegments) !== JSON.stringify(active.segments)),
  );

  function playFrom(seconds: number, endSeconds?: number) {
    if (!audioRef.current) return;
    playbackEndRef.current = endSeconds;
    audioRef.current.currentTime = seconds;
    void audioRef.current.play();
  }

  function boundPlayback() {
    const audio = audioRef.current;
    const end = playbackEndRef.current;
    if (!audio || end == null || audio.currentTime < end) return;
    audio.pause();
    audio.currentTime = end;
    playbackEndRef.current = undefined;
  }

  useEffect(() => {
    if (!models.some((model) => model.model_id === modelId))
      setModelId(models[0]?.model_id ?? "");
  }, [modelId, models]);

  useEffect(
    () => () => {
      if (audioUrl?.startsWith("blob:")) URL.revokeObjectURL(audioUrl);
    },
    [audioUrl],
  );

  useEffect(() => {
    if (!active?.source_path) {
      setAudioUrl(undefined);
      return;
    }
    let cancelled = false;
    setError(undefined);
    void loadTranscriptionAudio(active.source_path)
      .then((url) => {
        if (!cancelled) setAudioUrl(url);
      })
      .catch((caught) => {
        if (!cancelled) setError(caught instanceof Error ? caught.message : String(caught));
      });
    return () => {
      cancelled = true;
    };
  }, [active?.id, active?.source_path]);

  useEffect(() => {
    setDraftText(active?.text ?? "");
    setDraftSegments(active?.segments ?? []);
    setSpeakerLabels(active?.diarization?.labels ?? {});
    setSavedState(undefined);
  }, [active?.id]);

  function publishActive(updated: TranscriptionRecord) {
    setActive(updated);
    onChange(records.map((record) => (record.id === updated.id ? updated : record)));
  }

  async function selectRecord(record: TranscriptionRecord) {
    setActive(record);
    setAudioPath(record.source_path);
    setError(undefined);
  }

  async function run() {
    if (!modelId || !audioPath || running) return;
    setRunning(true);
    setError(undefined);
    try {
      const result = await transcribeAudio(modelId, audioPath, cleanup);
      setActive(result);
      onChange([
        result,
        ...records.filter((record) => record.id !== result.id),
      ]);
      setAudioPath(result.source_path);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setRunning(false);
    }
  }

  function updateSegment(index: number, text: string) {
    const next = draftSegments.map((segment, segmentIndex) =>
      segmentIndex === index ? { ...segment, text } : segment,
    );
    setDraftSegments(next);
    setDraftText(next.map((segment) => segment.text.trim()).filter(Boolean).join(" "));
    setSavedState(undefined);
  }

  function resetCorrection() {
    setDraftText(active?.text ?? "");
    setDraftSegments(active?.segments ?? []);
    setSavedState(undefined);
  }

  async function saveCorrection() {
    if (!active || !correctionDirty || saving) return;
    setSaving(true);
    setError(undefined);
    try {
      const correction = await updateTranscription(active.id, draftText, draftSegments);
      const updated = {
        ...active,
        ...correction,
        alignment: active.alignment ? { ...active.alignment, current: false } : null,
      };
      setActive(updated);
      onChange(records.map((record) => (record.id === updated.id ? updated : record)));
      setSavedState(`Revision ${updated.revision_count ?? 1} saved`);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setSaving(false);
    }
  }

  async function separateSpeakers() {
    const model = diarizationModels[0];
    if (!active || !model || diarizing || !active.words?.length) return;
    setDiarizing(true);
    setError(undefined);
    try {
      const diarization = await diarizeTranscription(
        active.id,
        model.model_id,
        speakerCount === "auto" ? undefined : Number(speakerCount),
      );
      const updated = { ...active, diarization };
      setSpeakerLabels(diarization.labels);
      publishActive(updated);
      setSavedState("Speaker separation completed");
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setDiarizing(false);
    }
  }

  async function alignWords() {
    const model = alignmentModels[0];
    if (!active || !model || aligning || correctionDirty || !draftSegments.length) return;
    setAligning(true);
    setError(undefined);
    try {
      const alignment = await alignTranscription(active.id, model.model_id);
      publishActive({ ...active, alignment });
      setSavedState(`Aligned revision ${alignment.source_revision}`);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setAligning(false);
    }
  }

  async function saveSpeakerLabels() {
    if (!active?.diarization || savingLabels) return;
    setSavingLabels(true);
    setError(undefined);
    try {
      const revision = await updateTranscriptionSpeakerLabels(active.id, speakerLabels);
      const updated = {
        ...active,
        diarization: { ...active.diarization, ...revision },
      };
      publishActive(updated);
      setSavedState(`Speaker labels revision ${revision.label_revision_count} saved`);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setSavingLabels(false);
    }
  }

  return (
    <div className="page transcribe-page">
      <PageHeader
        title="Transcribe"
        subtitle="Turn local audio into a persistent, timed transcript with an installed open-source model."
        actions={
          <button
            className="button button-primary"
            type="button"
            disabled={!modelId || !audioPath || running}
            onClick={() => void run()}
          >
            {running ? <LoaderCircle className="spin" size={14} /> : null}
            {running ? "Transcribing" : "Transcribe audio"}
          </button>
        }
      />
      {!models.length ? (
        <div className="model-notice is-danger">
          <StatusText tone="warning">
            Install Whisper Tiny from Models to enable transcription.
          </StatusText>
        </div>
      ) : null}
      <div className="transcribe-layout">
        <Panel className="transcribe-input" ariaLabel="Transcription input">
          <SelectField
            label="Speech-to-text model"
            value={modelId}
            onChange={setModelId}
            options={models.map((model) => ({
              value: model.model_id,
              label: model.model_id,
            }))}
          />
          <label className="toggle-row transcribe-toggle">
            <span>
              <strong>Speech cleanup</strong>
              <small>Derived copy; original stays unchanged</small>
            </span>
            <input
              aria-label="Speech cleanup"
              type="checkbox"
              checked={cleanup}
              disabled={running}
              onChange={(event) => setCleanup(event.target.checked)}
            />
          </label>
          <button
            className="sample-dropzone"
            type="button"
            onClick={async () =>
              setAudioPath((await pickAudioFile()) ?? audioPath)
            }
          >
            <span className="source-picker-icon"><Upload aria-hidden="true" size={16} /></span>
            <span className="source-picker-copy">
              <strong>{audioPath ? sourceName(audioPath) : "Choose an audio file"}</strong>
              <small>WAV, FLAC, MP3, M4A, or OGG · copied to local storage</small>
            </span>
            <span className="source-picker-action">Browse</span>
          </button>
          {error ? <StatusText tone="danger">{error}</StatusText> : null}
          <span className="section-label">Recent transcripts</span>
          <div className="transcription-list">
            {records.length ? (
              records.map((record) => (
                <button
                  className={record.id === active?.id ? "is-selected" : ""}
                  type="button"
                  key={record.id}
                  onClick={() => void selectRecord(record)}
                >
                  <strong>{sourceName(record.source_path)}</strong>
                  <span>
                    {record.text.slice(0, 80) || "No speech detected"}
                  </span>
                  <small>{new Date(record.created_at).toLocaleString()}</small>
                </button>
              ))
            ) : (
              <EmptyState
                title="No transcripts"
                detail="Completed transcripts persist here across restarts."
              />
            )}
          </div>
        </Panel>
        <Panel className="transcript-editor" ariaLabel="Transcript">
          {active ? (
            <>
              <audio
                ref={audioRef}
                className="visually-hidden"
                preload="metadata"
                src={audioUrl}
                onTimeUpdate={boundPlayback}
                onEnded={() => { playbackEndRef.current = undefined; }}
              />
              <MetricStrip
                metrics={[
                  {
                    value: `${active.audio_duration_seconds.toFixed(1)} s`,
                    label: "Audio",
                  },
                  {
                    value: `${active.inference_seconds.toFixed(2)} s`,
                    label: "Inference",
                    tone: "success",
                  },
                  {
                    value: `${active.rtf.toFixed(3)}x`,
                    label: "RTF",
                    tone: "success",
                  },
                ]}
              />
              <div
                className="transcription-evidence"
                aria-label="Transcription evidence"
              >
                <span>
                  <small>Language</small>
                  <strong>
                    {active.detected_language?.toUpperCase() ?? "Not reported"}
                    {active.language_confidence != null
                      ? ` / ${(active.language_confidence * 100).toFixed(1)}%`
                      : active.evidence?.language_source === "model-declared"
                        ? " / declared"
                        : ""}
                  </strong>
                </span>
                <span>
                  <small>Word timing</small>
                  <strong>
                    {active.words?.length
                      ? `${active.words.length} aligned`
                      : "Not reported"}
                  </strong>
                </span>
                <span>
                  <small>Word confidence</small>
                  <strong>
                    {measuredWordConfidence
                      ? `${measuredWordConfidence} measured`
                      : "Not reported"}
                  </strong>
                </span>
              </div>
              {active.processing?.algorithm &&
              active.processing.algorithm !== "none" ? (
                <div className="processing-receipt">
                  <span>Speech cleanup</span>
                  <strong>
                    {active.processing.noise_floor_before_dbfs?.toFixed(1) ??
                      "--"}{" "}
                    to{" "}
                    {active.processing.noise_floor_after_dbfs?.toFixed(1) ??
                      "--"}{" "}
                    dBFS
                  </strong>
                  <small>
                    {Math.round(
                      (active.processing.gated_frame_ratio ?? 0) * 100,
                    )}
                    % frames attenuated / original preserved
                  </small>
                </div>
              ) : null}
              <section className="alignment-analysis" aria-label="Forced word alignment">
                <div className="alignment-toolbar">
                  <div className="speaker-title">
                    <TextCursorInput size={14} />
                    <div>
                      <strong>Corrected word alignment</strong>
                      <small>English CTC / revision-linked</small>
                    </div>
                  </div>
                  <button
                    className="button"
                    type="button"
                    disabled={!alignmentModels.length || !draftSegments.length || correctionDirty || aligning}
                    onClick={() => void alignWords()}
                  >
                    {aligning ? <LoaderCircle className="spin" size={12} /> : <TextCursorInput size={12} />}
                    {aligning ? "Aligning" : active.alignment?.current ? "Run again" : "Align correction"}
                  </button>
                </div>
                {!alignmentModels.length ? <StatusText tone="warning">Install Wav2Vec2 Forced Alignment from Models to align corrected English text.</StatusText> : null}
                {correctionDirty ? <StatusText tone="muted">Save the correction before aligning it.</StatusText> : null}
                {active.alignment ? (
                  <>
                    <div className="alignment-receipt">
                      <span className={active.alignment.current ? "is-current" : "is-stale"}>{active.alignment.current ? `Revision ${active.alignment.source_revision}` : "Stale after correction"}</span>
                      <span>Provisional CTC path</span>
                      <span>Scores uncalibrated</span>
                      <span>{(active.alignment.mean_alignment_score * 100).toFixed(1)}% mean / {active.alignment.inference_seconds.toFixed(2)} s</span>
                    </div>
                    <div className="alignment-word-rail">
                      {active.alignment.words.map((word, index) => (
                        <button key={`${word.start_seconds}-${index}`} type="button" onClick={() => playFrom(word.start_seconds, word.end_seconds)} title={`Play aligned word / ${(word.alignment_score * 100).toFixed(1)}% uncalibrated path score`}>
                          <span>{word.text}</span>
                          <small>{timecode(word.start_seconds)} / {(word.alignment_score * 100).toFixed(0)}%</small>
                        </button>
                      ))}
                    </div>
                  </>
                ) : null}
              </section>
              <section className="speaker-analysis" aria-label="Speaker separation">
                <div className="speaker-toolbar">
                  <div className="speaker-title">
                    <UsersRound size={14} />
                    <div>
                      <strong>Speakers</strong>
                      <small>Word-anchored local clustering</small>
                    </div>
                  </div>
                  <SelectField
                    label="Speaker count"
                    value={speakerCount}
                    onChange={setSpeakerCount}
                    options={[
                      { value: "auto", label: "Auto" },
                      ...Array.from({ length: 8 }, (_, index) => ({
                        value: String(index + 1),
                        label: String(index + 1),
                      })),
                    ]}
                  />
                  <button
                    className="button"
                    type="button"
                    disabled={!diarizationModels.length || !active.words?.length || diarizing}
                    onClick={() => void separateSpeakers()}
                  >
                    {diarizing ? <LoaderCircle className="spin" size={12} /> : <UsersRound size={12} />}
                    {diarizing ? "Separating" : active.diarization ? "Run again" : "Separate speakers"}
                  </button>
                </div>
                {!diarizationModels.length ? (
                  <StatusText tone="warning">Install WavLM Speaker Verification from Models to separate speakers.</StatusText>
                ) : null}
                {active.diarization ? (
                  <>
                    <div className="speaker-receipt" aria-label="Speaker separation evidence">
                      <span>Provisional clustering</span>
                      <span>Overlap not detected</span>
                      <span>No turn confidence</span>
                      <span>{active.diarization.speakers.length} speakers / {active.diarization.inference_seconds.toFixed(2)} s</span>
                    </div>
                    <div className="speaker-label-grid">
                      {active.diarization.speakers.map((speaker) => (
                        <label key={speaker.id}>
                          <span>{speaker.default_name}</span>
                          <input
                            aria-label={`Name ${speaker.default_name}`}
                            value={speakerLabels[speaker.id] ?? speaker.default_name}
                            maxLength={80}
                            onChange={(event) => setSpeakerLabels((labels) => ({ ...labels, [speaker.id]: event.target.value }))}
                          />
                        </label>
                      ))}
                      <button
                        className="icon-button"
                        type="button"
                        title="Save speaker labels"
                        aria-label="Save speaker labels"
                        disabled={savingLabels || active.diarization.speakers.some((speaker) => !(speakerLabels[speaker.id] ?? "").trim())}
                        onClick={() => void saveSpeakerLabels()}
                      >
                        {savingLabels ? <LoaderCircle className="spin" size={12} /> : <Save size={12} />}
                      </button>
                    </div>
                    <div className="speaker-turn-list">
                      {active.diarization.turns.map((turn, index) => (
                        <div className="speaker-turn" key={`${turn.start_seconds}-${index}`}>
                          <button className="icon-button" type="button" title={`Play ${speakerLabels[turn.speaker_id] ?? turn.speaker_id}`} aria-label={`Play speaker turn ${index + 1}`} onClick={() => playFrom(turn.start_seconds, turn.end_seconds)}><Play size={11} /></button>
                          <strong>{speakerLabels[turn.speaker_id] ?? turn.speaker_id}</strong>
                          <small>{timecode(turn.start_seconds)}-{timecode(turn.end_seconds)}</small>
                          <span>{turn.text}</span>
                        </div>
                      ))}
                    </div>
                  </>
                ) : null}
              </section>
              <div className="transcript-copy">
                <div className="transcript-heading">
                  <div>
                    <h2>{sourceName(active.source_path)}</h2>
                    <small>{active.revision_count ? `${active.revision_count} correction revision${active.revision_count === 1 ? "" : "s"}` : "Model transcript"}</small>
                  </div>
                  <div className="transcript-actions">
                    <button className="icon-button" type="button" title="Reset unsaved correction" disabled={!correctionDirty || saving} onClick={resetCorrection}><RotateCcw size={12} /></button>
                    <button className="button button-primary" type="button" disabled={!correctionDirty || saving || !draftText.trim()} onClick={() => void saveCorrection()}>{saving ? <LoaderCircle className="spin" size={12} /> : <Save size={12} />}{saving ? "Saving" : "Save correction"}</button>
                  </div>
                </div>
                {savedState ? <StatusText tone="success">{savedState}</StatusText> : null}
                {!draftSegments.length ? <textarea className="transcript-textarea" aria-label="Corrected transcript" value={draftText} onChange={(event) => { setDraftText(event.target.value); setSavedState(undefined); }} /> : null}
              </div>
              {active.words?.length ? (
                <div className="word-timing">
                  <div className="word-timing-heading">
                    <span className="section-label">Word timing</span>
                    <small>
                      {evidenceLabel(active.evidence?.timing_source)}
                    </small>
                  </div>
                  <div className="word-timing-rail">
                    {active.words.map((word, index) => (
                  <button
                    type="button"
                    key={`${word.start_seconds}-${index}`}
                    aria-label={`Play from ${timecode(word.start_seconds)}: ${word.text}`}
                    title={`Play from ${timecode(word.start_seconds)}${word.confidence != null ? ` / ${(word.confidence * 100).toFixed(1)}% confidence` : ""}`}
                        onClick={() => playFrom(word.start_seconds)}
                      >
                        <span>{word.text}</span>
                        <small>{timecode(word.start_seconds)}</small>
                      </button>
                    ))}
                  </div>
                </div>
              ) : null}
              <div className="segment-list">
                {draftSegments.map((segment, index) => (
                  <div className="transcript-segment" key={`${segment.start_seconds}-${index}`}>
                    <button className="icon-button" type="button" title={`Play segment ${index + 1}`} onClick={() => playFrom(segment.start_seconds)}><Play size={11} /></button>
                    <span>{timecode(segment.start_seconds)}-{timecode(segment.end_seconds)}</span>
                    <textarea aria-label={`Transcript segment ${index + 1}`} value={segment.text} onChange={(event) => updateSegment(index, event.target.value)} />
                  </div>
                ))}
              </div>
            </>
          ) : (
            <EmptyState
              title="Select a transcript"
              detail="Choose a local audio file and run an installed STT model."
            />
          )}
        </Panel>
      </div>
    </div>
  );
}
