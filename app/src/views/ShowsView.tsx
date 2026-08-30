import { ArrowLeft, CircleCheck, CircleDashed, Clapperboard, LoaderCircle, Plus, TriangleAlert, UsersRound } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { EmptyState, PageHeader, Panel } from "../components/ui";
import { useVideoIntegration, useVideoProjectSummaries } from "../components/video/VideoIntegrationContext";
import { videoProjectStatusLabel } from "../components/video/VideoMasterCard";
import type { VideoProject, VideoReleasePlan, VideoShowFormat } from "../types/video";

/**
 * Shows and episodes.
 *
 * A format holds the decisions that do not change between episodes; an episode inherits them by
 * copy. Everything a multi-character production needs to be understood at a glance lives here -
 * who is in the cast, which lines are performed, and what a release is still waiting on - because
 * none of it is visible from a list of project names.
 */
export function ShowsView() {
  const { service, onOpenProject } = useVideoIntegration();
  const { projects, loading: projectsLoading } = useVideoProjectSummaries();
  const [formats, setFormats] = useState<VideoShowFormat[]>([]);
  const [formatsError, setFormatsError] = useState<string>();
  const [loadingFormats, setLoadingFormats] = useState(true);
  const [selectedId, setSelectedId] = useState<string>();
  const [episode, setEpisode] = useState<VideoProject>();
  const [release, setRelease] = useState<VideoReleasePlan>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();

  useEffect(() => {
    let active = true;
    if (!service) {
      setLoadingFormats(false);
      return;
    }
    service
      .listShowFormats()
      .then((saved) => { if (active) setFormats(saved); })
      .catch((caught) => { if (active) setFormatsError(caught instanceof Error ? caught.message : String(caught)); })
      .finally(() => { if (active) setLoadingFormats(false); });
    return () => { active = false; };
  }, [service]);

  // Episodes are ordinary Video Studio projects; a format's episodes are the ones that recorded it
  // as their origin, which is provenance the manifest already carries.
  const loadEpisode = useCallback(async (projectId: string) => {
    if (!service) return;
    setBusy(true);
    setError(undefined);
    try {
      const loaded = await service.getVideoProject(projectId);
      setEpisode(loaded);
      setRelease(await service.planEpisodeRelease(projectId, false));
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  }, [service]);

  useEffect(() => {
    if (selectedId) void loadEpisode(selectedId);
  }, [selectedId, loadEpisode]);

  const dialogue = episode?.manifest.dialogue ?? [];
  const cast = episode?.manifest.cast ?? [];
  const performed = dialogue.filter((turn) => turn.narrated && !turn.draft).length;
  const drafts = dialogue.filter((turn) => turn.draft).length;

  if (!service) {
    return <div className="page">
      <PageHeader title="Shows" subtitle="Reusable series formats and the episodes made from them." />
      <EmptyState title="Video Studio is unavailable" detail="Shows are built on Video Studio, which is not available in this build." />
    </div>;
  }

  // An episode gets its own screen. Reading a cast and a script against a table of other episodes
  // is reading two things at once, and the episode is the one being looked at.
  if (selectedId) return <div className="page episode-page">
    <PageHeader
      title={episode?.name ?? "Episode"}
      subtitle={episode?.manifest.format_origin
        ? `From ${episode.manifest.format_origin.format_name} revision ${episode.manifest.format_origin.format_revision}`
        : "Not started from a saved format"}
      actions={<>
        <button className="button button-secondary" type="button" onClick={() => { setSelectedId(undefined); setEpisode(undefined); setRelease(undefined); }}>
          <ArrowLeft aria-hidden="true" size={14} />All shows
        </button>
        <button className="button button-primary" type="button" onClick={() => onOpenProject?.(selectedId)}>
          <Clapperboard aria-hidden="true" size={13} />Open in Video Studio
        </button>
      </>}
    />
    {busy ? <div className="video-library-loading" role="status"><LoaderCircle className="spin" aria-hidden="true" size={14} />Reading the episode</div> : null}
    {error ? <p className="shows-error" role="alert">{error}</p> : null}
    {!busy && !error ? <div className="episode-screen">
      <Panel ariaLabel="Cast">
        <h3 className="shows-section-heading"><UsersRound aria-hidden="true" size={13} />Cast</h3>
        {cast.length ? <ul className="video-cast-list">
          {cast.map((member) => <li key={member.id}><strong>{member.display_name}</strong><small>{member.voice_id} · {member.language}</small></li>)}
        </ul> : <p className="shows-panel-empty">No cast yet. Ask the assistant for a multi-character script.</p>}
      </Panel>

      <Panel ariaLabel="Script">
        <h3 className="shows-section-heading">Script</h3>
        {dialogue.length ? <>
          {/* The three line states are what decide what to do next. */}
          <p className="shows-counts">
            <span><CircleCheck aria-hidden="true" size={12} />{performed} performed</span>
            <span><TriangleAlert aria-hidden="true" size={12} />{drafts} draft</span>
            <span><CircleDashed aria-hidden="true" size={12} />{dialogue.length - performed - drafts} not narrated</span>
          </p>
          <ol className="video-dialogue-list">
            {dialogue.map((turn) => <li key={turn.id} className={`is-${turn.draft ? "draft" : turn.narrated ? "final" : "pending"}`}>
              <span className="video-dialogue-speaker">{cast.find((member) => member.id === turn.character_id)?.display_name ?? turn.character_id}</span>
              <span className="video-dialogue-text">{turn.text}</span>
            </li>)}
          </ol>
        </> : <p className="shows-panel-empty">No script yet.</p>}
      </Panel>

      <Panel ariaLabel="Release">
        <h3 className="shows-section-heading">Release</h3>
        {release ? <ul className="shows-release-list">
          {release.members.map((member) => <li key={member.kind} className={member.ready ? "is-ready" : ""}>
            <span>{member.ready ? <CircleCheck aria-hidden="true" size={12} /> : <CircleDashed aria-hidden="true" size={12} />}</span>
            <strong>{member.kind.replace(/_/g, " ")}</strong>
            {/* A blocked member names its missing prerequisite rather than being quietly absent. */}
            {member.blocked_reason ? <small>{member.blocked_reason}</small> : null}
          </li>)}
        </ul> : <p className="shows-panel-empty">Release readiness is unavailable for this episode.</p>}
      </Panel>
    </div> : null}
  </div>;

  return <div className="page shows-page">
    <PageHeader
      title="Shows"
      subtitle="A format holds what does not change between episodes. Each episode inherits it by copy, so editing a show never rewrites one you already published."
    />

    <Panel className="table-panel" ariaLabel="Saved show formats">
      <div className="project-table-controls">
        <strong className="shows-section-title">Formats</strong>
        <span className="project-table-count">{formats.length} saved</span>
      </div>
      {loadingFormats ? <div className="video-library-loading" role="status"><LoaderCircle className="spin" aria-hidden="true" size={14} />Loading formats</div>
        : formats.length ? <table className="project-table">
          <thead><tr><th scope="col">Show</th><th scope="col">Cast</th><th scope="col">Captions</th><th scope="col">Loudness</th><th scope="col">Revision</th></tr></thead>
          <tbody>
            {formats.map((format) => <tr key={format.id}>
              <td><strong>{format.name}</strong></td>
              <td>{format.cast.map((member) => member.display_name).join(", ") || "—"}</td>
              <td>{format.caption_preset_id}</td>
              <td>{(format.target_lufs_milli / 1000).toFixed(1)} LUFS</td>
              <td>r{format.revision}</td>
            </tr>)}
          </tbody>
        </table>
        : <p className="shows-panel-empty">
            {formatsError ?? "No show formats yet. Ask the assistant to save one once an episode sounds the way you want, and every later episode starts from it."}
          </p>}
    </Panel>

    <Panel className="table-panel" ariaLabel="Episodes">
      <div className="project-table-controls">
        <strong className="shows-section-title">Episodes</strong>
        <span className="project-table-count">{projects.length} production{projects.length === 1 ? "" : "s"}</span>
      </div>
      {projectsLoading && !projects.length ? <div className="video-library-loading" role="status"><LoaderCircle className="spin" aria-hidden="true" size={14} />Loading episodes</div>
        : projects.length ? <table className="project-table">
          <thead><tr><th scope="col">Episode</th><th scope="col">Scenes</th><th scope="col">Status</th><th scope="col">Updated</th></tr></thead>
          <tbody>
            {projects.map((project) => <tr
              key={project.id}
              className={project.id === selectedId ? "is-selected" : ""}
              tabIndex={0}
              role="button"
              aria-label={`Inspect ${project.name}`}
              onClick={() => setSelectedId(project.id)}
              onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") { event.preventDefault(); setSelectedId(project.id); } }}
            >
              <td><strong>{project.name}</strong></td>
              <td>{project.scene_count}</td>
              <td>{videoProjectStatusLabel(project)}</td>
              <td>{new Date(project.updated_at).toLocaleDateString()}</td>
            </tr>)}
          </tbody>
        </table>
        : <p className="shows-panel-empty">No episodes yet. Start one in Video Studio, or ask the assistant to create an episode from a saved format.</p>}
    </Panel>

    <p className="shows-footnote">
      <Plus aria-hidden="true" size={12} />
      Casts, scripts, pronunciation, score, and sound design are authored through the assistant, and
      every change lands on the same revision-checked project the timeline uses.
    </p>
  </div>;
}
