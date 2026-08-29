import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import type { VideoProjectSummary, VideoStudioService } from "../../types/video";

interface VideoIntegrationValue {
  service?: VideoStudioService;
  revision: number;
  activeProjectId?: string;
  onOpenProject?: (projectId: string) => void;
  /** Show a finished project's player without entering the editor. */
  onPreviewProject?: (projectId: string) => void;
}

const VideoIntegrationContext = createContext<VideoIntegrationValue>({ revision: 0 });

export function VideoIntegrationProvider({
  children,
  onOpenProject,
  onPreviewProject,
  activeProjectId,
  revision = 0,
  service,
}: {
  children: ReactNode;
  onOpenProject: (projectId: string) => void;
  onPreviewProject?: (projectId: string) => void;
  activeProjectId?: string;
  revision?: number;
  service: VideoStudioService;
}) {
  const value = useMemo(
    () => ({ service, revision, activeProjectId, onOpenProject, onPreviewProject }),
    [activeProjectId, onOpenProject, onPreviewProject, revision, service],
  );
  return <VideoIntegrationContext.Provider value={value}>{children}</VideoIntegrationContext.Provider>;
}

export function useVideoIntegration() {
  return useContext(VideoIntegrationContext);
}

/**
 * Save an export through the desktop shell.
 *
 * Downloads cannot be anchors: the media origin is cross-origin to the app, so the webview ignores
 * the `download` attribute and navigates the window to the file, leaving a blank media document
 * with no way back. Every download surface routes through here instead.
 */
export function useArtifactSaver() {
  const { service } = useVideoIntegration();
  const [saving, setSaving] = useState(false);

  const save = useCallback(async (localPath?: string, suggestedName?: string) => {
    if (!service || !localPath) throw new Error("This export is no longer available on disk.");
    setSaving(true);
    try {
      return await service.saveArtifact(localPath, suggestedName);
    } finally {
      setSaving(false);
    }
  }, [service]);

  return { save, saving };
}

export function useVideoProjectSummaries(enabled = true) {
  const { revision, service } = useVideoIntegration();
  const [projects, setProjects] = useState<VideoProjectSummary[]>([]);
  const [loading, setLoading] = useState(Boolean(service && enabled));
  const [error, setError] = useState<string>();

  const refresh = useCallback(async () => {
    if (!service || !enabled) {
      setProjects([]);
      setLoading(false);
      return [];
    }
    setLoading(true);
    try {
      const next = await service.listVideoProjects();
      setProjects(next);
      setError(undefined);
      return next;
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
      return [];
    } finally {
      setLoading(false);
    }
  }, [enabled, service]);

  useEffect(() => {
    let active = true;
    if (!service || !enabled) {
      setLoading(false);
      return () => { active = false; };
    }
    setLoading(true);
    void service.listVideoProjects().then((next) => {
      if (!active) return;
      setProjects(next);
      setError(undefined);
    }).catch((caught) => {
      if (active) setError(caught instanceof Error ? caught.message : String(caught));
    }).finally(() => {
      if (active) setLoading(false);
    });
    return () => { active = false; };
  }, [enabled, revision, service]);

  return { projects, loading, error, refresh };
}
