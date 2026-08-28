import { CircleStop, FileAudio2, Mic, ShieldCheck, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { getAudioRecordingStatus, importVoiceProfile, pickAudioFile, startAudioRecording, stopAudioRecording } from "../lib/bridge";
import type { AudioRecordingState, VoiceProfile } from "../types";
import { Dropdown, StatusText } from "./ui";

export function VoiceProfileDialog({
  onClose,
  onCreated,
}: {
  onClose: () => void;
  onCreated: (voice: VoiceProfile) => void;
}) {
  const [name, setName] = useState("");
  const [style, setStyle] = useState("");
  const [samplePath, setSamplePath] = useState("");
  const [relationship, setRelationship] = useState("self");
  const [consentBasis, setConsentBasis] = useState("");
  const [permittedUses, setPermittedUses] = useState("Personal and commercial speech generation");
  const [sourceDate, setSourceDate] = useState(() => new Date().toISOString().slice(0, 10));
  const [acknowledged, setAcknowledged] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string>();
  const [recording, setRecording] = useState<AudioRecordingState>({ recording: false });
  const recordingActive = useRef(false);

  useEffect(() => {
    if (!recording.recording) return;
    const timer = window.setInterval(() => void getAudioRecordingStatus().then((status) => {
      setRecording(status);
      if (!status.recording && status.audio_path) setSamplePath(status.audio_path);
      if (status.capture_error) setError(status.capture_error);
    }).catch((caught) => setError(caught instanceof Error ? caught.message : String(caught))), 100);
    return () => window.clearInterval(timer);
  }, [recording.recording]);

  useEffect(() => {
    recordingActive.current = recording.recording;
  }, [recording.recording]);

  useEffect(() => () => {
    if (recordingActive.current) void stopAudioRecording().catch(() => undefined);
  }, []);

  async function toggleRecording() {
    setError(undefined);
    try {
      if (recording.recording) {
        const completed = await stopAudioRecording();
        setRecording(completed);
        if (!completed.audio_path) throw new Error("The recording stopped without producing a voice sample.");
        setSamplePath(completed.audio_path);
      } else {
        setSamplePath("");
        setRecording(await startAudioRecording({ vad_enabled: true, auto_stop: false, silence_ms: 1200, input_gain: 1 }));
      }
    } catch (caught) {
      setRecording({ recording: false });
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function uploadSample() {
    setError(undefined);
    try {
      const path = await pickAudioFile();
      if (path) setSamplePath(path);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function close() {
    if (recording.recording) {
      try { await stopAudioRecording(); } catch { /* Closing must not strand microphone capture. */ }
    }
    setRecording({ recording: false });
    onClose();
  }

  async function createProfile() {
    if (!name.trim() || !samplePath || !acknowledged || !consentBasis.trim() || !permittedUses.trim()) return;
    setSaving(true);
    setError(undefined);
    try {
      const voice = await importVoiceProfile({
        name: name.trim(),
        style: style.trim() || "Custom voice",
        source_path: samplePath,
        consent_confirmed: acknowledged,
        consent_basis: consentBasis.trim(),
        speaker_relationship: relationship,
        permitted_uses: permittedUses.trim(),
        source_date: sourceDate,
      });
      onCreated(voice);
      onClose();
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="modal-backdrop" role="presentation" onMouseDown={() => void close()}>
      <div className="modal" role="dialog" aria-modal="true" aria-labelledby="add-voice-title" onMouseDown={(event) => event.stopPropagation()}>
        <div className="modal-header">
          <div><h2 id="add-voice-title">Add voice profile</h2><p>Reference audio and consent metadata remain on this machine.</p></div>
          <button className="icon-button" type="button" title="Close" onClick={() => void close()}><X aria-hidden="true" size={15} /></button>
        </div>
        <div className="modal-body">
          <label className="form-field"><span>Name</span><input autoFocus value={name} onChange={(event) => setName(event.target.value)} placeholder="Voice name" /></label>
          <label className="form-field"><span>Style</span><input value={style} onChange={(event) => setStyle(event.target.value)} placeholder="Warm documentary" /></label>
          <div className="sample-source">
            <span className="field-label">Reference audio</span>
            <div className="sample-source-actions">
              <button className={`button ${recording.recording ? "button-secondary danger-button" : "button-primary"}`} type="button" onClick={() => void toggleRecording()}>{recording.recording ? <CircleStop aria-hidden="true" size={13} /> : <Mic aria-hidden="true" size={13} />}{recording.recording ? "Stop recording" : "Record sample"}</button>
              <button className="button button-secondary" type="button" disabled={recording.recording} onClick={() => void uploadSample()}><FileAudio2 aria-hidden="true" size={13} />Upload audio</button>
            </div>
            <div className={`sample-source-status ${recording.recording ? "is-recording" : ""}`}>
              <strong>{recording.recording ? "Recording from microphone" : samplePath.split("/").at(-1) || "No sample selected"}</strong>
              <span>{recording.recording ? `${(recording.duration_seconds ?? 0).toFixed(1)} sec` : samplePath ? "Ready to analyze" : "WAV, FLAC, MP3, M4A, or OGG"}</span>
            </div>
          </div>
          <label className="form-field"><span>Speaker relationship</span><Dropdown ariaLabel="Speaker relationship" value={relationship} onChange={setRelationship} options={[{ value: "self", label: "My own voice" }, { value: "authorized-person", label: "Authorized speaker" }, { value: "licensed-source", label: "Licensed source" }]} /></label>
          <label className="form-field"><span>Consent basis</span><input value={consentBasis} onChange={(event) => setConsentBasis(event.target.value)} placeholder="Recorded by me, or written permission details" /></label>
          <label className="form-field"><span>Permitted uses</span><input value={permittedUses} onChange={(event) => setPermittedUses(event.target.value)} /></label>
          <label className="form-field"><span>Source date</span><input type="date" value={sourceDate} onChange={(event) => setSourceDate(event.target.value)} /></label>
          <label className="consent-note consent-check"><input type="checkbox" checked={acknowledged} onChange={(event) => setAcknowledged(event.target.checked)} /><ShieldCheck aria-hidden="true" size={16} /><span>I confirm I own this voice or have explicit permission to create and use its synthetic likeness.</span></label>
          {error ? <StatusText tone="danger">{error}</StatusText> : null}
        </div>
        <div className="modal-actions">
          <button className="button button-secondary" type="button" onClick={() => void close()}>Cancel</button>
          <button className="button button-primary" type="button" disabled={saving || !name.trim() || !samplePath || !acknowledged || !consentBasis.trim() || !permittedUses.trim()} onClick={() => void createProfile()}>{saving ? "Importing and analyzing..." : "Create profile"}</button>
        </div>
      </div>
    </div>
  );
}
