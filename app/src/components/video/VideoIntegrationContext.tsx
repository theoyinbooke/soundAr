import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import type { VideoProjectSummary, VideoStudioService } from "../../types/video";

interface VideoIntegrationValue {
  service?: VideoStudioService;
  revision: number;
  activeProjectId?: string;
  onOpenProject?: (projectId: string) => void;
}

const VideoIntegrationContext = createContext<VideoIntegrationValue>({ revision: 0 });

export function VideoIntegrationProvider({
  children,
  onOpenProject,
  activeProjectId,
  revision = 0,
  service,
}: {
  children: ReactNode;
  onOpenProject: (projectId: string) => void;
  activeProjectId?: string;
  revision?: number;
  service: VideoStudioService;
}) {
  const value = useMemo(() => ({ service, revision, activeProjectId, onOpenProject }), [activeProjectId, onOpenProject, revision, service]);
  return <VideoIntegrationContext.Provider value={value}>{children}</VideoIntegrationContext.Provider>;
}

export function useVideoIntegration() {
  return useContext(VideoIntegrationContext);
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
