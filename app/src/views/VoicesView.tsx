import { FileAudio2, Play, Plus, Search, ShieldCheck, X } from "lucide-react";
import { useDeferredValue, useMemo, useRef, useState } from "react";
import type { VoiceProfile } from "../types";
import { PageHeader, Panel, Segmented, StatusText } from "../components/ui";

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
  voices,
  onChange,
}: {
  voices: VoiceProfile[];
  onChange: (voices: VoiceProfile[]) => void;
}) {
  const [query, setQuery] = useState("");
  const deferredQuery = useDeferredValue(query.trim().toLowerCase());
  const [filter, setFilter] = useState<VoiceFilter>("all");
  const [selectedId, setSelectedId] = useState(voices[0]?.id ?? "");
  const [showAdd, setShowAdd] = useState(false);
  const [newName, setNewName] = useState("");
  const [newStyle, setNewStyle] = useState("");
  const [sampleName, setSampleName] = useState("");
  const fileInput = useRef<HTMLInputElement>(null);

  const filtered = useMemo(() => {
    return voices.filter((voice) => {
      if (filter === "verified" && voice.consent !== "confirmed") return false;
      if (filter === "draft" && voice.state !== "draft") return false;
      if (!deferredQuery) return true;
      return [voice.name, voice.style, voice.sample_label, ...voice.engines].join(" ").toLowerCase().includes(deferredQuery);
    });
  }, [deferredQuery, filter, voices]);

  const selected = voices.find((voice) => voice.id === selectedId) ?? voices[0];

  function addVoice() {
    if (!newName.trim()) return;
    const voice: VoiceProfile = {
      id: crypto.randomUUID(),
      name: newName.trim(),
      style: newStyle.trim() || "Custom voice",
      sample_label: sampleName || "Sample pending",
      sample_seconds: sampleName ? 18 : 0,
      engines: ["Chatterbox", "XTTS"],
      consent: "pending",
      state: "draft",
      color: "coral",
    };
    onChange([...voices, voice]);
    setSelectedId(voice.id);
    setShowAdd(false);
    setNewName("");
    setNewStyle("");
    setSampleName("");
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
                  <th aria-label="Preview" />
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
                      <button className="icon-button table-play" type="button" title={`Preview ${voice.name}`} onClick={(event) => event.stopPropagation()}>
                        <Play aria-hidden="true" fill="currentColor" size={12} />
                      </button>
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
              <button className="text-button" type="button"><Play aria-hidden="true" size={11} /> Preview</button>
            </div>

            <span className="section-label inspector-section">Provenance</span>
            <dl className="compact-definition-list">
              <div><dt>Source</dt><dd>{selected.state === "preset" ? "Built in" : "Owner recorded"}</dd></div>
              <div><dt>Consent</dt><dd><StatusText tone={selected.consent === "confirmed" ? "success" : "warning"}>{selected.consent}</StatusText></dd></div>
              <div><dt>Storage</dt><dd>Local only</dd></div>
              <div><dt>Profile</dt><dd>{selected.state}</dd></div>
            </dl>

            <span className="section-label inspector-section">Engine coverage</span>
            <div className="engine-coverage">
              {selected.engines.map((engine) => <div key={engine}><strong>{engine}</strong><StatusText tone="success">Ready</StatusText></div>)}
              {!selected.engines.includes("Kokoro") ? <div><strong>Kokoro</strong><StatusText tone="warning">Preset only</StatusText></div> : null}
            </div>

            <div className="inspector-bottom-actions">
              <button className="button button-secondary" type="button">Edit profile</button>
              <button className="button button-primary" type="button">Use voice</button>
            </div>
          </Panel>
        ) : null}
      </div>

      {showAdd ? (
        <div className="modal-backdrop" role="presentation" onMouseDown={() => setShowAdd(false)}>
          <div className="modal" role="dialog" aria-modal="true" aria-labelledby="add-voice-title" onMouseDown={(event) => event.stopPropagation()}>
            <div className="modal-header">
              <div><h2 id="add-voice-title">Add voice profile</h2><p>Reference audio and consent metadata remain on this machine.</p></div>
              <button className="icon-button" type="button" title="Close" onClick={() => setShowAdd(false)}><X aria-hidden="true" size={15} /></button>
            </div>
            <div className="modal-body">
              <label className="form-field"><span>Name</span><input autoFocus value={newName} onChange={(event) => setNewName(event.target.value)} placeholder="Voice name" /></label>
              <label className="form-field"><span>Style</span><input value={newStyle} onChange={(event) => setNewStyle(event.target.value)} placeholder="Warm documentary" /></label>
              <input ref={fileInput} className="visually-hidden" type="file" accept="audio/*" onChange={(event) => setSampleName(event.target.files?.[0]?.name ?? "")} />
              <button className="sample-dropzone" type="button" onClick={() => fileInput.current?.click()}>
                <FileAudio2 aria-hidden="true" size={20} />
                <strong>{sampleName || "Choose a clean voice sample"}</strong>
                <span>WAV, FLAC, or MP3 / 10-30 seconds recommended</span>
              </button>
              <div className="consent-note"><ShieldCheck aria-hidden="true" size={16} /><span>Only add voices you own or have permission to use.</span></div>
            </div>
            <div className="modal-actions">
              <button className="button button-secondary" type="button" onClick={() => setShowAdd(false)}>Cancel</button>
              <button className="button button-primary" type="button" disabled={!newName.trim()} onClick={addVoice}>Create profile</button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
