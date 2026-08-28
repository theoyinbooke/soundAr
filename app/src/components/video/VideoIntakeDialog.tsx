import { Check, ChevronDown, FileAudio2, FileVideo2, Link2, LoaderCircle, Upload, X } from "lucide-react";
import { useEffect, useId, useRef, useState, type DragEvent, type FormEvent, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import type {
  CreateVideoProjectRequest,
  ImportLinkRequest,
  ImportLocalVideoRequest,
  LocalAudioSelection,
  LocalVideoSelection,
  VideoLinkPreview,
  VideoStudioEntry,
  VideoToolStatus,
} from "../../types/video";
import { formatVideoClock } from "../../lib/videoState";

interface VideoIntakeDialogProps {
  entry: VideoStudioEntry;
  tools: VideoToolStatus[];
  onClose: () => void;
  onPreviewLink: (exactUrl: string) => Promise<VideoLinkPreview>;
  onPickLocalVideo?: () => Promise<LocalVideoSelection | undefined>;
  onPickLocalAudio?: () => Promise<LocalAudioSelection | undefined>;
  onImportLink: (request: ImportLinkRequest) => Promise<void>;
  onImportLocalVideo: (request: ImportLocalVideoRequest) => Promise<void>;
  onCreateVideo: (request: CreateVideoProjectRequest) => Promise<void>;
}

const focusable = "button:not(:disabled), input:not(:disabled), textarea:not(:disabled), select:not(:disabled), [href], [tabindex]:not([tabindex='-1'])";

function validUrl(value: string): boolean {
  try { return /^https?:$/.test(new URL(value).protocol); } catch { return false; }
}

function fileSelection(file: File): LocalVideoSelection {
  return { file, display_name: file.name, size_bytes: file.size };
}

export function VideoIntakeDialog({
  entry,
  tools,
  onClose,
  onPreviewLink,
  onPickLocalVideo,
  onPickLocalAudio,
  onImportLink,
  onImportLocalVideo,
  onCreateVideo,
}: VideoIntakeDialogProps) {
  const titleId = useId();
  const descriptionId = useId();
  const dialogRef = useRef<HTMLDivElement>(null);
  const firstFieldRef = useRef<HTMLInputElement | HTMLTextAreaElement>(null);
  const uploadInputRef = useRef<HTMLInputElement>(null);
  const audioInputRef = useRef<HTMLInputElement>(null);
  const restoreFocusRef = useRef<HTMLElement | null>(null);
  const [url, setUrl] = useState("");
  const [rightsConfirmed, setRightsConfirmed] = useState(false);
  const [localRightsConfirmed, setLocalRightsConfirmed] = useState(false);
  const [linkPreview, setLinkPreview] = useState<VideoLinkPreview>();
  const [selection, setSelection] = useState<LocalVideoSelection>();
  const [prompt, setPrompt] = useState("");
  const [audioSelection, setAudioSelection] = useState<LocalAudioSelection>();
  const [previewing, setPreviewing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  useEffect(() => {
    restoreFocusRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    firstFieldRef.current?.focus();
    const frame = window.requestAnimationFrame(() => firstFieldRef.current?.focus());
    return () => {
      window.cancelAnimationFrame(frame);
      restoreFocusRef.current?.focus();
    };
  }, []);

  useEffect(() => {
    setRightsConfirmed(false);
    setLinkPreview(undefined);
    setError(undefined);
    if (!validUrl(url)) return;
    let active = true;
    const timer = window.setTimeout(() => {
      setPreviewing(true);
      void onPreviewLink(url).then((preview) => {
        if (active && preview.exact_url === url) setLinkPreview(preview);
      }).catch((caught) => {
        if (active) setError(caught instanceof Error ? caught.message : String(caught));
      }).finally(() => {
        if (active) setPreviewing(false);
      });
    }, 220);
    return () => { active = false; window.clearTimeout(timer); };
  }, [onPreviewLink, url]);

  function trapFocus(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key === "Escape") {
      event.preventDefault();
      if (!busy) onClose();
      return;
    }
    if (event.key !== "Tab") return;
    const elements = [...(dialogRef.current?.querySelectorAll<HTMLElement>(focusable) ?? [])].filter((element) => element.getClientRects().length > 0);
    if (!elements.length) return;
    const first = elements[0];
    const last = elements.at(-1)!;
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  }

  function acceptDrop(event: DragEvent<HTMLDivElement>) {
    event.preventDefault();
    const file = [...event.dataTransfer.files].find((item) => item.type.startsWith("video/"));
    if (file) selectVideo(fileSelection(file));
  }

  function selectVideo(next: LocalVideoSelection) {
    setSelection(next);
    setLocalRightsConfirmed(false);
  }

  async function chooseVideo() {
    setError(undefined);
    if (!onPickLocalVideo) {
      uploadInputRef.current?.click();
      return;
    }
    try {
      const nativeSelection = await onPickLocalVideo();
      if (nativeSelection) selectVideo(nativeSelection);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function chooseAudio() {
    setError(undefined);
    if (!onPickLocalAudio) {
      audioInputRef.current?.click();
      return;
    }
    try {
      const nativeSelection = await onPickLocalAudio();
      if (nativeSelection) setAudioSelection(nativeSelection);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      if (entry === "link") {
        if (!linkPreview || !rightsConfirmed || linkPreview.exact_url !== url) throw new Error("Preview and confirm rights for this exact URL.");
        await onImportLink({ exact_url: url, rights_confirmed: true, rights_confirmation_url: url, single_source_only: true });
      } else if (entry === "upload") {
        if (!selection || !localRightsConfirmed) throw new Error("Choose a local video and confirm you are authorized to use it.");
        await onImportLocalVideo({ ...selection, rights_confirmed: true });
      } else {
        if (!prompt.trim() && !audioSelection) throw new Error("Add a prompt or choose an audio source.");
        await onCreateVideo({
          prompt: prompt.trim(),
          ...(audioSelection ? {
            audio_file: audioSelection.file,
            audio_local_path: audioSelection.local_path,
            audio_display_name: audioSelection.display_name,
          } : {}),
        });
      }
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  }

  const title = entry === "link" ? "Import a video link" : entry === "upload" ? "Upload a local video" : "Start from prompt or audio";
  const canContinue = entry === "link"
    ? Boolean(linkPreview && rightsConfirmed && linkPreview.exact_url === url)
    : entry === "upload"
      ? Boolean(selection && localRightsConfirmed)
      : Boolean(prompt.trim() || audioSelection);

  return createPortal(
    <div className="video-modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget && !busy) onClose(); }}>
      <div ref={dialogRef} className="video-intake-dialog" role="dialog" aria-modal="true" aria-labelledby={titleId} aria-describedby={descriptionId} onKeyDown={trapFocus}>
        <header className="video-dialog-header">
          <div><h2 id={titleId}>{title}</h2><p id={descriptionId}>Sources and generated media stay in this local project.</p></div>
          <button className="video-icon-button" type="button" aria-label="Close source intake" disabled={busy} onClick={onClose}><X aria-hidden="true" size={16} /></button>
        </header>
        <form onSubmit={(event) => void submit(event)}>
          <div className="video-dialog-body">
            {entry === "link" ? <>
              <label className="video-field"><span>Video URL</span><input ref={firstFieldRef as React.RefObject<HTMLInputElement>} aria-label="Video URL" type="url" value={url} placeholder="https://www.youtube.com/watch?v=…" onChange={(event) => setUrl(event.target.value)} /></label>
              <p className="video-field-help">soundAr imports one video only. Playlists, channels, and collections are not supported.</p>
              <section className="video-source-preview" aria-label="Source preview">
                <span className="video-section-label">Preview</span>
                {previewing ? <div className="video-preview-loading" role="status"><LoaderCircle className="video-spin" aria-hidden="true" size={15} />Checking this exact URL…</div> : linkPreview ? <div className="video-link-preview">
                  <video src={linkPreview.preview_url} muted playsInline preload="metadata" aria-label={`Preview of ${linkPreview.title}`} />
                  <div><strong>{linkPreview.title}</strong><span>{linkPreview.creator}</span><small>{formatVideoClock(linkPreview.duration_ms)} · {linkPreview.view_label} · {linkPreview.published_label}</small></div>
                </div> : <div className="video-preview-empty"><Link2 aria-hidden="true" size={17} /><span>Enter one exact video URL to preview it.</span></div>}
              </section>
              <label className="video-rights-check"><input type="checkbox" checked={rightsConfirmed} disabled={!linkPreview || linkPreview.exact_url !== url} onChange={(event) => setRightsConfirmed(event.target.checked)} /><span>I have the rights or permission to use this exact URL.</span></label>
              <details className="video-tool-disclosure" open>
                <summary>Media tools <ChevronDown aria-hidden="true" size={13} /></summary>
                <div>{tools.map((tool) => <div key={tool.id}><span>{tool.label}</span><span className={`video-tool-state is-${tool.state}`}>{tool.state === "ready" ? "Ready" : tool.state === "setup-needed" ? "Setup needed" : "Unavailable"}<i aria-hidden="true" /></span></div>)}</div>
              </details>
            </> : null}

            {entry === "upload" ? <>
              <div ref={firstFieldRef as React.RefObject<HTMLDivElement>} className={`video-upload-zone ${selection ? "has-selection" : ""}`} tabIndex={0} role="button" aria-label="Choose a local video" onClick={() => void chooseVideo()} onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); void chooseVideo(); } }} onDragOver={(event) => event.preventDefault()} onDrop={acceptDrop}>
                {selection ? <><Check aria-hidden="true" size={20} /><strong>{selection.display_name}</strong><span>{selection.size_bytes ? `${(selection.size_bytes / 1_048_576).toFixed(1)} MB` : "Ready to analyze locally"}</span></> : <><Upload aria-hidden="true" size={21} /><strong>Choose or drop a video</strong><span>MP4, MOV, MKV, WebM, or M4V</span></>}
              </div>
              <input ref={uploadInputRef} className="video-visually-hidden" tabIndex={-1} type="file" accept="video/*,.mkv,.m4v" onChange={(event) => { const file = event.target.files?.[0]; if (file) selectVideo(fileSelection(file)); }} />
              <label className="video-rights-check"><input type="checkbox" checked={localRightsConfirmed} disabled={!selection} onChange={(event) => setLocalRightsConfirmed(event.target.checked)} /><span>I own this media or have permission to use it in this project.</span></label>
            </> : null}

            {entry === "prompt" ? <>
              <label className="video-field"><span>Describe the video</span><textarea ref={firstFieldRef as React.RefObject<HTMLTextAreaElement>} aria-label="Video prompt" rows={5} value={prompt} placeholder="Create a calm portrait video from this narration…" onChange={(event) => setPrompt(event.target.value)} /></label>
              <div className="video-audio-source">
                <div><FileAudio2 aria-hidden="true" size={17} /><span><strong>{audioSelection?.display_name ?? "Optional audio source"}</strong><small>Use existing speech, music, or a local audio file.</small></span></div>
                <button className="video-button is-secondary" type="button" onClick={() => void chooseAudio()}>{audioSelection ? "Change audio" : "Choose audio"}</button>
                <input ref={audioInputRef} className="video-visually-hidden" tabIndex={-1} type="file" accept="audio/*" onChange={(event) => { const file = event.target.files?.[0]; if (file) setAudioSelection({ file, display_name: file.name, size_bytes: file.size }); }} />
              </div>
            </> : null}

            {error ? <div className="video-inline-error" role="alert">{error}</div> : null}
          </div>
          <footer className="video-dialog-actions">
            <button className="video-button is-secondary" type="button" disabled={busy} onClick={onClose}>Cancel</button>
            <button className="video-button is-primary" type="submit" disabled={busy || !canContinue}>{busy ? <LoaderCircle className="video-spin" aria-hidden="true" size={14} /> : entry === "upload" ? <FileVideo2 aria-hidden="true" size={14} /> : null}{busy ? "Starting…" : entry === "link" ? "Review source" : entry === "upload" ? "Analyze video" : "Create project"}</button>
          </footer>
        </form>
      </div>
    </div>,
    document.body,
  );
}
