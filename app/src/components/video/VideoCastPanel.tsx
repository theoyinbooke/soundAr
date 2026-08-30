import { useMemo, useState } from "react";
import { CircleCheck, CircleDashed, LoaderCircle, Mic, TriangleAlert } from "lucide-react";
import type { VideoProject } from "../../types/video";

/**
 * The cast and script surface.
 *
 * A multi-character episode is read line by line, so this shows the state of each line rather than
 * of the project as a whole: which lines have a finished take, which are still standing in with a
 * draft, and which have never been performed. Those three states are what decide what to do next,
 * and none of them is visible anywhere else in the editor.
 */
export function VideoCastPanel({
  project,
  onNarrate,
  onPromote,
  narrating,
  panelId,
  labelledBy,
}: {
  project: VideoProject;
  onNarrate: (turnIds: string[], draft: boolean) => Promise<void>;
  onPromote: (turnIds: string[]) => Promise<void>;
  narrating: boolean;
  panelId: string;
  labelledBy: string;
}) {
  const [selected, setSelected] = useState<string>();
  const cast = project.manifest.cast ?? [];
  const dialogue = project.manifest.dialogue ?? [];

  const characters = useMemo(
    () => new Map(cast.map((member) => [member.id, member])),
    [cast],
  );
  const pending = dialogue.filter((turn) => !turn.narrated).map((turn) => turn.id);
  const drafts = dialogue.filter((turn) => turn.draft).map((turn) => turn.id);

  const lexicon = project.manifest.lexicon ?? [];
  const cues = project.manifest.music_cues ?? [];
  const soundLayers = project.manifest.sound_layers ?? [];
  const soundAssets = project.manifest.sound_assets ?? [];
  const origin = project.manifest.format_origin;

  if (!cast.length) {
    return (
      <div id={panelId} role="tabpanel" aria-labelledby={labelledBy} className="video-cast-panel">
        <p className="video-cast-empty">
          This episode has no cast yet. Ask the assistant for a multi-character script and it will
          bind each character to its own voice, time the conversation, and list every line here.
        </p>
      </div>
    );
  }

  return (
    <div id={panelId} role="tabpanel" aria-labelledby={labelledBy} className="video-cast-panel">
      {origin ? (
        <p className="video-cast-notice">
          From <strong>{origin.format_name}</strong> revision {origin.format_revision}. These values
          were copied when the episode started, so editing the show will not change this episode.
        </p>
      ) : null}
      <section aria-label="Cast">
        <h4>Cast</h4>
        <ul className="video-cast-list">
          {cast.map((member) => (
            <li key={member.id}>
              <strong>{member.display_name}</strong>
              <small>
                {member.voice_id} · {member.language}
              </small>
            </li>
          ))}
        </ul>
      </section>

      <section aria-label="Script">
        <h4>
          Script <small>{dialogue.length} line(s)</small>
        </h4>
        <ol className="video-dialogue-list">
          {dialogue.map((turn) => {
            const speaker = characters.get(turn.character_id);
            const state = turn.draft ? "draft" : turn.narrated ? "final" : "pending";
            return (
              <li key={turn.id} className={`is-${state}`}>
                <button
                  type="button"
                  aria-pressed={selected === turn.id}
                  onClick={() => setSelected(selected === turn.id ? undefined : turn.id)}
                >
                  <span className="video-dialogue-state" aria-hidden="true">
                    {state === "final" ? (
                      <CircleCheck size={13} />
                    ) : state === "draft" ? (
                      <TriangleAlert size={13} />
                    ) : (
                      <CircleDashed size={13} />
                    )}
                  </span>
                  <span className="video-dialogue-speaker">{speaker?.display_name ?? turn.character_id}</span>
                  <span className="video-dialogue-text">{turn.text}</span>
                  {turn.direction ? <em className="video-dialogue-direction">({turn.direction})</em> : null}
                  {/* A stand-in is called what it is, so it is never mistaken for finished work. */}
                  <span className="video-dialogue-badge">
                    {state === "final" ? "Performed" : state === "draft" ? "Draft take" : "Not narrated"}
                  </span>
                </button>
              </li>
            );
          })}
        </ol>
      </section>

      {lexicon.length ? (
        <section aria-label="Pronunciation">
          <h4>
            Pronunciation <small>{lexicon.length} rule(s)</small>
          </h4>
          <ul className="video-cast-list">
            {lexicon.map((entry) => (
              <li key={entry.id}>
                <strong>
                  {entry.match_text} → {entry.replacement}
                </strong>
                {/* Scope is what decides which lines a change re-reads. */}
                <small>
                  {entry.scope === "character"
                    ? characters.get(entry.character_id ?? "")?.display_name ?? entry.character_id
                    : entry.scope}
                  {entry.matching === "exact" ? " · case-sensitive" : ""}
                </small>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      {cues.length ? (
        <section aria-label="Score">
          <h4>
            Score <small>{cues.length} cue(s)</small>
          </h4>
          <ul className="video-cast-list">
            {cues.map((cue) => (
              <li key={cue.id}>
                <strong>
                  {cue.role} · {Math.round(cue.target_duration_ms / 1000)}s
                </strong>
                <small>
                  {cue.needs_generation ? "Not composed yet" : "Composed and placed"} · {cue.direction}
                </small>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      {soundLayers.length ? (
        <section aria-label="Sound design">
          <h4>
            Sound design <small>{soundLayers.length} placement(s)</small>
          </h4>
          <ul className="video-cast-list">
            {soundLayers.map((layer) => (
              <li key={layer.id}>
                <strong>
                  {soundAssets.find((asset) => asset.id === layer.asset_id)?.name ?? layer.asset_id}
                </strong>
                <small>
                  {layer.kind.replace("_", " ")} · {Math.round(layer.start_ms / 1000)}s–
                  {Math.round(layer.end_ms / 1000)}s
                </small>
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      <div className="video-cast-actions">
        <button
          type="button"
          disabled={narrating || !pending.length}
          onClick={() => onNarrate(pending, false)}
        >
          {narrating ? <LoaderCircle className="spin" size={13} /> : <Mic size={13} />}
          <span>Narrate {pending.length} remaining</span>
        </button>
        <button
          type="button"
          disabled={narrating || !pending.length}
          onClick={() => onNarrate(pending, true)}
        >
          <span>Draft all remaining</span>
        </button>
        <button
          type="button"
          disabled={narrating || !drafts.length}
          onClick={() => onPromote(drafts)}
        >
          <span>Promote {drafts.length} draft(s)</span>
        </button>
        {selected ? (
          <button type="button" disabled={narrating} onClick={() => onPromote([selected])}>
            <span>Re-read this line</span>
          </button>
        ) : null}
      </div>

      {drafts.length ? (
        <p className="video-cast-notice">
          {drafts.length} line(s) are still draft takes. A master cannot be published, and a release
          cannot be exported, until they are promoted.
        </p>
      ) : null}
    </div>
  );
}
