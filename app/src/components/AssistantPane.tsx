import { ArrowUp, Bot, Check, ChevronDown, CircleAlert, CircleStop, Clapperboard, Download, ExternalLink, FileVideo2, LoaderCircle, MessageCircle, MoreHorizontal, PanelRightClose, Pause, Play, Plus, ShieldCheck, X } from "lucide-react";
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { codexRequest, listenToCodex, loadAssistantVideoThreadLink, loadCodexModels, refreshCodexConnection, respondToCodex, type AgentAccess, type CodexEvent, type CodexModel, type CodexStatus, type ReasoningEffort } from "../lib/codexBridge";
import { exportHistoryItem, listHistory, listJobs, loadGeneratedAudio, loadJobPreview } from "../lib/bridge";
import type { HistoryItem, JobRecord } from "../types";
import type { VideoArtifact, VideoJobPhase, VideoProject } from "../types/video";
import { useArtifactSaver, useVideoIntegration, useVideoProjectSummaries } from "./video/VideoIntegrationContext";
import { videoSourceForIdlePoster } from "../lib/videoPlayback";

type Message = { id: string; role: "user" | "assistant" | "system"; text: string; pending?: boolean };
type ToolRun = { id: string; title: string; detail: string; state: "running" | "complete" | "failed" };
type Approval = { id: number; method: string; title: string; detail: string };
type ThreadSummary = { id: string; preview?: string; name?: string; updatedAt?: number };
type PlanStep = { step: string; status: "pending" | "inProgress" | "completed" };
type VideoPhaseRun = { phase: VideoJobPhase; title: string; detail: string; state: "running" | "complete" | "failed" };

const defaultMessages: Message[] = [{ id: "welcome", role: "assistant", text: "Tell me what you are building—even if the idea is unfinished. I can research and plan it, write the content, create speech or music, assemble projects and batches, and revise the result with you." }];

export function AssistantPane({ open, onClose, onStudioChanged, variant = "rail" }: { open: boolean; onClose: () => void; onStudioChanged?: () => void; variant?: "rail" | "canvas" }) {
  const { onOpenProject: onOpenVideoProject, service: videoService } = useVideoIntegration();
  const { refresh: refreshVideoProjects } = useVideoProjectSummaries(open);
  const [status, setStatus] = useState<CodexStatus>({ available: true, connected: false });
  const [account, setAccount] = useState<Record<string, unknown>>();
  const [models, setModels] = useState<CodexModel[]>([]);
  const [modelId, setModelId] = useState("");
  const [effort, setEffort] = useState<ReasoningEffort>("high");
  const [access, setAccess] = useState<AgentAccess>("workspace-write");
  const [threadId, setThreadId] = useState<string>();
  const [turnId, setTurnId] = useState<string>();
  const [messages, setMessages] = useState<Message[]>(defaultMessages);
  const [toolRuns, setToolRuns] = useState<ToolRun[]>([]);
  const [planSteps, setPlanSteps] = useState<PlanStep[]>([]);
  const [approval, setApproval] = useState<Approval>();
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string>();
  const [connecting, setConnecting] = useState(true);
  const [connectionRefresh, setConnectionRefresh] = useState(0);
  const [sending, setSending] = useState(false);
  const [menu, setMenu] = useState<"model" | "effort" | "access">();
  const [threadMenuOpen, setThreadMenuOpen] = useState(false);
  const [threads, setThreads] = useState<ThreadSummary[]>([]);
  const [artifacts, setArtifacts] = useState<HistoryItem[]>([]);
  const [activeJobs, setActiveJobs] = useState<JobRecord[]>([]);
  const [artifactMode, setArtifactMode] = useState<"single" | "project">("single");
  const [videoPhases, setVideoPhases] = useState<VideoPhaseRun[]>([]);
  const [videoProject, setVideoProject] = useState<VideoProject>();
  const [videoResult, setVideoResult] = useState<VideoArtifact>();
  const scrollRef = useRef<HTMLDivElement>(null);
  const initialHistoryIds = useRef<Set<string> | undefined>(undefined);
  const initialJobIds = useRef<Set<string> | undefined>(undefined);
  const artifactKey = useRef("");
  const composerRef = useRef<HTMLTextAreaElement>(null);

  useLayoutEffect(() => {
    // Reset the transient phase while hidden so a previously unavailable result cannot paint for
    // one frame when the persistent pane is opened again.
    if (!open) {
      setConnecting(true);
      return;
    }
    let active = true;
    let unlisten: () => void = () => undefined;
    setConnecting(true);
    setError(undefined);
    refreshCodexConnection()
      .then(async (current) => {
        if (!active) return;
        setStatus(current);
        if (!current.connected) {
          setAccount(undefined);
          setModels([]);
          return;
        }
        const [accountResponse, loadedModels] = await Promise.all([
          codexRequest<{ account?: Record<string, unknown>; requiresOpenaiAuth: boolean }>("account/read", { refreshToken: false }),
          loadCodexModels(),
        ]);
        if (!active) return;
        setAccount(accountResponse.account);
        setModels(loadedModels);
        const threadResponse = await codexRequest<{ data: ThreadSummary[] }>("thread/list", { cwd: current.studio_root ?? null, sourceKinds: ["appServer"], sortKey: "updated_at", sortDirection: "desc", limit: 20 });
        if (active) setThreads(threadResponse.data);
        const selected = loadedModels.find((model) => model.isDefault) ?? loadedModels[0];
        if (selected) {
          setModelId((value) => value || selected.id);
          setEffort(selected.defaultReasoningEffort);
        }
        unlisten = await listenToCodex(handleEvent);
      })
      .catch((caught) => active && setError(caught instanceof Error ? caught.message : String(caught)))
      .finally(() => active && setConnecting(false));
    return () => { active = false; unlisten(); };
  }, [open, connectionRefresh]);

  useEffect(() => {
    if (!open) return;
    let active = true;
    let timer = 0;
    async function refreshArtifacts() {
      try {
        const [history, jobs] = await Promise.all([listHistory(), listJobs()]);
        if (!active) return;
        if (!initialHistoryIds.current) initialHistoryIds.current = new Set(history.map((item) => item.id));
        if (!initialJobIds.current) initialJobIds.current = new Set(jobs.map((item) => item.id));
        setActiveJobs(selectAssistantJobs(jobs, initialJobIds.current));
        const created = selectAssistantArtifacts(history, initialHistoryIds.current, artifactMode);
        if (created.length) {
          const key = created.map((item) => item.id).join(":");
          if (key !== artifactKey.current) {
            artifactKey.current = key;
            setArtifacts(created);
            onStudioChanged?.();
          }
        }
      } finally {
        if (active) timer = window.setTimeout(refreshArtifacts, sending || activeJobs.length ? 500 : 3000);
      }
    }
    void refreshArtifacts();
    return () => { active = false; window.clearTimeout(timer); };
  }, [open, sending, activeJobs.length, onStudioChanged, artifactMode]);

  useEffect(() => {
    const element = scrollRef.current;
    if (element && typeof element.scrollTo === "function") element.scrollTo({ top: element.scrollHeight, behavior: "smooth" });
  }, [messages, toolRuns, activeJobs, artifacts, approval, videoPhases, videoProject, videoResult]);

  const selectedModel = useMemo(() => models.find((model) => model.id === modelId) ?? models[0], [modelId, models]);
  const efforts = selectedModel?.supportedReasoningEfforts?.map((option) => option.reasoningEffort) ?? ["low", "medium", "high"];

  async function resolveVideoResult(item: Record<string, unknown>, tool: string) {
    if (!videoService) return;
    const embedded = videoProjectFromToolResult(item);
    const projectId = videoProjectIdFromToolResult(item) ?? embedded?.id;
    const summaries = await refreshVideoProjects();
    const summary = projectId
      ? summaries.find((project) => project.id === projectId)
      : videoPhaseForTool(tool) === "export"
        ? summaries.find((project) => project.master)
        : summaries[0];
    try {
      const resolved = projectId || summary?.id ? await videoService.getVideoProject(projectId ?? summary!.id) : embedded;
      if (resolved) {
        setVideoProject(resolved);
        setVideoResult(resolved.master ?? preferredVideoResult(resolved, videoPhaseForTool(tool)));
      }
    } catch {
      if (embedded) {
        setVideoProject(embedded);
        setVideoResult(embedded.master ?? preferredVideoResult(embedded, videoPhaseForTool(tool)));
      }
    }
    onStudioChanged?.();
  }

  function handleEvent(event: CodexEvent) {
    const params = event.params ?? {};
    if (event.method === "item/agentMessage/delta") {
      const delta = typeof params.delta === "string" ? params.delta : "";
      setMessages((items) => {
        const last = items.at(-1);
        if (last?.role === "assistant" && last.pending) return [...items.slice(0, -1), { ...last, text: last.text + delta }];
        return [...items, { id: String(params.itemId ?? Date.now()), role: "assistant", text: delta, pending: true }];
      });
    } else if (event.method === "turn/completed") {
      setSending(false);
      setTurnId(undefined);
      setMessages((items) => items.map((message) => ({ ...message, pending: false })));
    } else if (event.method === "turn/started") {
      const turn = params.turn as Record<string, unknown> | undefined;
      if (typeof turn?.id === "string") setTurnId(turn.id);
    } else if (event.method === "turn/plan/updated") {
      const plan = Array.isArray(params.plan) ? params.plan : [];
      setPlanSteps(plan.flatMap((item) => {
        if (!item || typeof item !== "object") return [];
        const value = item as Record<string, unknown>;
        if (typeof value.step !== "string" || !["pending", "inProgress", "completed"].includes(String(value.status))) return [];
        return [{ step: value.step, status: value.status as PlanStep["status"] }];
      }));
    } else if (event.method === "item/started" || event.method === "item/completed") {
      const item = params.item as Record<string, unknown> | undefined;
      if (item?.type === "dynamicToolCall") {
        const id = String(item.id);
        const tool = String(item.tool ?? "soundAr tool");
        const phase = videoPhaseForTool(tool);
        const completed = event.method === "item/completed";
        const failed = completed && (item.status === "failed" || Boolean(item.error));
        if (phase) {
          const run: VideoPhaseRun = { phase, title: videoPhaseTitle(phase), detail: failed ? String((item.error as Record<string, unknown> | undefined)?.message ?? item.error ?? "Needs attention") : completed ? "Finished in soundAr" : "Working in soundAr", state: failed ? "failed" : completed ? "complete" : "running" };
          setVideoPhases((runs) => upsertVideoPhase(runs, run));
          if (completed && !failed) void resolveVideoResult(item, tool);
        } else if (isVideoTool(tool)) {
          if (completed && !failed) void resolveVideoResult(item, tool);
        } else {
          const title = humanize(tool);
          if (item.tool === "save_project" || item.tool === "export_project_master") setArtifactMode("project");
          setToolRuns((runs) => [...runs.filter((run) => run.id !== id), { id, title, detail: completed ? "Finished in soundAr" : "Working in soundAr", state: completed ? "complete" : "running" }]);
          if (completed) onStudioChanged?.();
        }
      } else if (event.method === "item/completed" && item?.type === "agentMessage" && item.phase === "final_answer") {
        setSending(false);
        setMessages((items) => items.map((message) => ({ ...message, pending: false })));
      }
    } else if (event.id && event.method.includes("requestApproval")) {
      const command = Array.isArray(params.command) ? params.command.join(" ") : String(params.command ?? "This operation needs additional access.");
      setApproval({ id: event.id, method: event.method, title: event.method.includes("fileChange") ? "Allow file changes?" : "Allow this operation?", detail: command });
    } else if (event.method === "error") {
      setError(String((params.error as Record<string, unknown> | undefined)?.message ?? params.message ?? "Codex reported an error."));
      setSending(false);
    } else if (event.method === "soundar/codex-disconnected") {
      setStatus((current) => ({ ...current, connected: false }));
      setError("The Codex connection stopped. Reconnect to continue this conversation.");
    } else if (event.method === "account/updated" || event.method === "account/login/completed") {
      void codexRequest<{ account?: Record<string, unknown> }>("account/read", { refreshToken: false }).then((response) => setAccount(response.account));
    }
  }

  async function startLogin() {
    setError(undefined);
    try {
      const login = await codexRequest<{ authUrl?: string; verificationUrl?: string }>("account/login/start", { type: "chatgpt", useHostedLoginSuccessPage: true, appBrand: "chatgpt" });
      const url = login.authUrl ?? login.verificationUrl;
      if (url) await openUrl(url);
    } catch (caught) { setError(caught instanceof Error ? caught.message : String(caught)); }
  }

  function retryCodexConnection() {
    setConnecting(true);
    setError(undefined);
    setConnectionRefresh((value) => value + 1);
  }

  async function send() {
    const text = draft.trim();
    if (!text || sending || !status.connected || !account) return;
    setDraft("");
    setError(undefined);
    setSending(true);
    setPlanSteps([]);
    setToolRuns([]);
    setArtifacts([]);
    setActiveJobs([]);
    setArtifactMode("single");
    setVideoPhases([]);
    setVideoProject(undefined);
    setVideoResult(undefined);
    const [currentHistory, currentJobs] = await Promise.all([
      listHistory().catch(() => []),
      listJobs().catch(() => []),
    ]);
    initialHistoryIds.current = new Set(currentHistory.map((item) => item.id));
    initialJobIds.current = new Set(currentJobs.map((item) => item.id));
    artifactKey.current = "";
    setMessages((items) => [...items, { id: `user-${Date.now()}`, role: "user", text }]);
    try {
      let activeThread = threadId;
      if (!activeThread) {
        const response = await codexRequest<{ thread: { id: string } }>("thread/start", {
          cwd: null,
          model: modelId || null,
          approvalPolicy: "on-request",
          approvalsReviewer: "user",
          sandbox: access,
          soundarAccess: access,
          ephemeral: false,
          personality: "pragmatic",
          threadSource: "user",
        });
        activeThread = response.thread.id;
        setThreadId(activeThread);
      }
      const response = await codexRequest<{ turn: { id: string } }>("turn/start", {
        threadId: activeThread,
        input: [{ type: "text", text }],
        model: modelId || null,
        effort,
        approvalPolicy: "on-request",
        approvalsReviewer: "user",
        sandboxPolicy: sandboxPolicy(access),
        soundarAccess: access,
      });
      setTurnId(response.turn.id);
      if (import.meta.env.DEV && !window.__TAURI_INTERNALS__) {
        window.setTimeout(() => {
          const preview = previewCreativeResponse(text);
          setMessages((items) => [...items, { id: `assistant-${Date.now()}`, role: "assistant", text: preview.message }]);
          setPlanSteps(preview.plan);
          setToolRuns(preview.tools);
          if (preview.video) {
            setVideoPhases([
              { phase: "source", title: "Source", detail: "Confirmed in soundAr", state: "complete" },
              { phase: "analyze", title: "Analyze", detail: "Source clock preserved", state: "complete" },
              { phase: "review", title: "Plan & revise", detail: "Scenes approved", state: "complete" },
              { phase: "preview", title: "Preview", detail: "Playable locally", state: "complete" },
              { phase: "export", title: "Export", detail: "Final master ready", state: "complete" },
            ]);
            void resolveVideoResult({ output: { project_id: "creator-update-master" } }, "export_video");
          }
          setSending(false);
        }, 650);
      }
    } catch (caught) {
      setSending(false);
      setError(caught instanceof Error ? caught.message : String(caught));
    }
  }

  async function interrupt() {
    if (!threadId || !turnId) return;
    await codexRequest("turn/interrupt", { threadId, turnId }).catch((caught) => setError(String(caught)));
  }

  async function answerApproval(approved: boolean) {
    if (!approval) return;
    const decision = approved ? "accept" : "decline";
    await respondToCodex(approval.id, { decision });
    setApproval(undefined);
  }

  async function resumeThread(id: string) {
    setThreadMenuOpen(false);
    setError(undefined);
    try {
      const response = await codexRequest<{ thread: Record<string, unknown> }>("thread/resume", { threadId: id, model: modelId || null, approvalPolicy: "on-request", approvalsReviewer: "user", sandbox: access, soundarAccess: access });
      // The native link resolver accepts only a thread successfully resumed in this authenticated
      // app-server session, so recovery deliberately follows (rather than races) thread/resume.
      const savedVideo = await loadAssistantVideoThreadLink(id);
      setThreadId(id);
      setMessages(readThreadMessages(response.thread));
      setToolRuns([]);
      setPlanSteps([]);
      setArtifacts([]);
      setActiveJobs([]);
      if (savedVideo && videoService) {
        try {
          const restoredProject = await videoService.getVideoProject(savedVideo.project_id);
          const exactLinkedResult = savedVideo.output_id
            ? [...(restoredProject.deliverables ?? []), ...restoredProject.manifest.artifacts]
                .find((artifact) => artifact.id === savedVideo.output_id && artifact.role === savedVideo.relationship)
            : undefined;
          setVideoProject(restoredProject);
          setVideoResult(exactLinkedResult ? restoredProject.master ?? exactLinkedResult : undefined);
          setVideoPhases(restoredVideoPhases(restoredProject));
        } catch {
          setVideoProject(undefined);
          setVideoResult(undefined);
          setVideoPhases([]);
        }
      } else {
        setVideoProject(undefined);
        setVideoResult(undefined);
        setVideoPhases([]);
      }
      initialHistoryIds.current = undefined;
      initialJobIds.current = undefined;
      artifactKey.current = "";
    } catch (caught) { setError(caught instanceof Error ? caught.message : String(caught)); }
  }

  if (!open) return null;

  const canvas = variant === "canvas";
  const idle = canvas && !messages.some((message) => message.role === "user");

  function newConversation() {
    setThreadId(undefined);
    setMessages(defaultMessages);
    setToolRuns([]);
    setPlanSteps([]);
    setArtifacts([]);
    setActiveJobs([]);
    setVideoPhases([]);
    setVideoProject(undefined);
    setVideoResult(undefined);
    initialHistoryIds.current = undefined;
    initialJobIds.current = undefined;
  }

  const threadMenu = threadMenuOpen ? <div className="assistant-thread-menu" role="menu" aria-label="Assistant conversations"><strong>Recent conversations</strong>{threads.length ? threads.map((thread) => <button role="menuitem" type="button" key={thread.id} onClick={() => void resumeThread(thread.id)}><span>{thread.name || thread.preview || "Untitled conversation"}</span><small>{thread.updatedAt ? new Date(thread.updatedAt * 1000).toLocaleDateString() : "Saved by Codex"}</small></button>) : <span>No saved conversations yet.</span>}</div> : null;

  const banners = <>
    {!connecting && !status.available ? <div className="assistant-banner is-warning"><CircleAlert size={17} /><div><strong>Codex CLI not detected</strong><p>{status.message}</p><button type="button" onClick={retryCodexConnection}>Scan again</button></div></div> : null}
    {status.available && !status.connected && !connecting ? <div className="assistant-banner is-warning"><CircleAlert size={17} /><div><strong>Could not connect to Codex</strong><p>{error ?? "The detected Codex installation did not start."}</p><button type="button" onClick={retryCodexConnection}>Reconnect</button></div></div> : null}
    {connecting ? <div className="assistant-loading"><LoaderCircle className="spin" size={18} /><span>Connecting to Codex…</span></div> : null}
    {status.connected && !account && !connecting ? <div className="assistant-signin"><Bot size={25} /><strong>Connect your ChatGPT account</strong><p>soundAr uses the login managed by your existing Codex installation. Your credentials stay with Codex.</p><button className="primary-button" type="button" onClick={() => void startLogin()}>Sign in with ChatGPT <ExternalLink size={13} /></button></div> : null}
  </>;

  const conversation = account ? <>
    {messages.map((message) => <article className={`assistant-message is-${message.role}`} key={message.id}><span>{message.role === "assistant" ? "Assistant" : message.role === "user" ? "You" : "soundAr"}</span><p>{message.text}{message.pending ? <i className="assistant-caret" /> : null}</p></article>)}
    {planSteps.length ? <ol className="assistant-plan" aria-label="Current plan">{planSteps.map((step, index) => <li className={`is-${step.status}`} key={`${index}-${step.step}`}><span>{step.status === "completed" ? <Check size={12} /> : step.status === "inProgress" ? <LoaderCircle className="spin" size={12} /> : index + 1}</span><strong>{step.step}</strong></li>)}</ol> : null}
    {videoPhases.length ? <AssistantVideoPhaseSummary phases={videoPhases} /> : null}
    {videoProject && videoResult ? <AssistantVideoResult artifact={videoResult} project={videoProject} onOpen={() => onOpenVideoProject?.(videoProject.id)} /> : null}
    {toolRuns.length ? <ActivitySummary runs={toolRuns} /> : null}
    {activeJobs.length ? <AssistantJobProgress jobs={activeJobs} mode={artifactMode} /> : null}
    {artifacts.map((item) => <AudioArtifact key={item.id} item={item} onRevise={() => {
      setDraft(`Revise “${item.title || (item.generation_kind === "music" ? "Generated music" : "Generated speech")}” (${item.id}): `);
      window.setTimeout(() => composerRef.current?.focus(), 0);
    }} />)}
    {approval ? <div className="assistant-approval"><ShieldCheck size={18} /><div><strong>{approval.title}</strong><p>{approval.detail}</p><div><button type="button" onClick={() => void answerApproval(false)}>Deny</button><button className="primary-button" type="button" onClick={() => void answerApproval(true)}>Allow once</button></div></div></div> : null}
  </> : null;

  const inlineError = error && status.connected ? <div className="assistant-inline-error"><CircleAlert size={14} /><span>{error}</span><button type="button" aria-label="Dismiss error" onClick={() => setError(undefined)}><X size={13} /></button></div> : null;

  const composer = <>
    <div className="assistant-composer">
      <textarea ref={composerRef} aria-label="Message soundAr assistant" placeholder={account ? "Ask soundAr to create anything" : "Connect Codex to begin"} value={draft} disabled={!status.connected || !account || sending} onChange={(event) => setDraft(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void send(); } }} />
      <div className="assistant-composer-bar">
        <div className="assistant-control-cluster is-context">
          <div className="assistant-control-slot is-model">
            <button type="button" className="assistant-control" aria-haspopup="menu" aria-expanded={menu === "model"} disabled={!models.length} onClick={() => setMenu(menu === "model" ? undefined : "model")}><span>{selectedModel?.displayName ?? "Model"}</span><ChevronDown size={12} /></button>
            {menu === "model" ? <div className="assistant-picker picker-model" role="menu" aria-label="Model">{models.map((model) => <button type="button" role="menuitemradio" aria-checked={model.id === modelId} key={model.id} onClick={() => { setModelId(model.id); setEffort(model.defaultReasoningEffort); setMenu(undefined); }}><span><strong>{model.displayName}</strong><small>{model.description}</small></span>{model.id === modelId ? <Check size={14} /> : null}</button>)}</div> : null}
          </div>
          <div className="assistant-control-slot is-access">
            <button type="button" className="assistant-control is-access" aria-haspopup="menu" aria-expanded={menu === "access"} onClick={() => setMenu(menu === "access" ? undefined : "access")}><ShieldCheck size={12} /><span>{access === "danger-full-access" ? "Full access" : access === "workspace-write" ? "Studio access" : "Read only"}</span><ChevronDown size={12} /></button>
            {menu === "access" ? <div className="assistant-picker picker-access" role="menu" aria-label="Access level">{(["read-only", "workspace-write", "danger-full-access"] as AgentAccess[]).map((value) => <button type="button" role="menuitemradio" aria-checked={value === access} key={value} onClick={() => { setAccess(value); setMenu(undefined); }}><span><strong>{value === "danger-full-access" ? "Full access" : value === "workspace-write" ? "Studio access" : "Read only"}</strong><small>{value === "danger-full-access" ? "Can use the machine with approvals" : value === "workspace-write" ? "Can research and manage soundAr work" : "Can inspect and plan only"}</small></span>{value === access ? <Check size={14} /> : null}</button>)}</div> : null}
          </div>
        </div>
        <div className="assistant-control-cluster is-actions">
          <div className="assistant-control-slot is-effort">
            <button type="button" className="assistant-control" aria-haspopup="menu" aria-expanded={menu === "effort"} disabled={!selectedModel} onClick={() => setMenu(menu === "effort" ? undefined : "effort")}><span>{humanize(effort)}</span><ChevronDown size={12} /></button>
            {menu === "effort" ? <div className="assistant-picker picker-effort" role="menu" aria-label="Reasoning effort">{efforts.map((value) => <button type="button" role="menuitemradio" aria-checked={value === effort} key={value} onClick={() => { setEffort(value); setMenu(undefined); }}><span><strong>{humanize(value)}</strong><small>{effortDescription(value)}</small></span>{value === effort ? <Check size={14} /> : null}</button>)}</div> : null}
          </div>
          <button className="assistant-send" type="button" aria-label={sending ? "Stop response" : "Send message"} disabled={!sending && (!draft.trim() || !status.connected || !account)} onClick={() => sending ? void interrupt() : void send()}>{sending ? <CircleStop size={16} /> : <ArrowUp size={16} />}</button>
        </div>
      </div>
    </div>
    <span className="assistant-disclaimer">Codex can make mistakes. Review actions before approval.</span>
  </>;

  if (canvas) {
    return <section className={`assistant-canvas ${idle ? "is-idle" : ""}`} aria-label="soundAr assistant">
      <div className="assistant-canvas-controls">
        <button type="button" aria-label="New conversation" title="New conversation" onClick={newConversation}><Plus size={16} /></button>
        <button type="button" aria-label="Conversation history" title="Conversation history" aria-expanded={threadMenuOpen} onClick={() => setThreadMenuOpen((value) => !value)}><MoreHorizontal size={16} /></button>
        {threadMenu}
      </div>
      <div className="assistant-thread" ref={scrollRef}>
        <div className="assistant-column">
          {banners}
          {idle ? <div className="assistant-hero"><h1>Hello</h1><p>Create something amazing.</p></div> : conversation}
          {inlineError}
        </div>
      </div>
      <footer className="assistant-composer-wrap">
        <div className="assistant-column">{composer}</div>
      </footer>
    </section>;
  }

  return <aside className="assistant-pane" aria-label="soundAr assistant">
    <header className="assistant-header">
      <div><strong>Assistant</strong><span>{account ? "Powered by your Codex login" : "Codex connection"}</span></div>
      <div className="assistant-header-actions"><button type="button" aria-label="New conversation" title="New conversation" onClick={newConversation}><Plus size={16} /></button><button type="button" aria-label="Conversation history" title="Conversation history" aria-expanded={threadMenuOpen} onClick={() => setThreadMenuOpen((value) => !value)}><MoreHorizontal size={16} /></button><button type="button" aria-label="Close assistant" title="Close assistant" onClick={onClose}><PanelRightClose size={16} /></button></div>
      {threadMenu}
    </header>
    <div className="assistant-thread" ref={scrollRef}>
      {banners}
      {conversation}
      {inlineError}
    </div>
    <footer className="assistant-composer-wrap">{composer}</footer>
  </aside>;
}

export function AssistantLauncher({ onClick }: { onClick: () => void }) {
  return <button className="assistant-launcher" type="button" aria-label="Open soundAr assistant" title="Open assistant" onClick={onClick}><MessageCircle size={19} /></button>;
}

function AssistantVideoPhaseSummary({ phases }: { phases: VideoPhaseRun[] }) {
  const failed = phases.some((phase) => phase.state === "failed");
  const running = phases.some((phase) => phase.state === "running");
  return <section className={`assistant-video-phases${failed ? " is-failed" : ""}`} aria-label="Video production progress" aria-live="polite">
    <div className="assistant-video-phases-heading"><span>{running ? <LoaderCircle className="spin" size={13} /> : failed ? <CircleAlert size={13} /> : <Check size={13} />}</span><strong>{failed ? "Video production needs attention" : running ? "Producing video locally" : "Video production complete"}</strong></div>
    <ol>{phases.map((phase) => <li className={`is-${phase.state}`} key={phase.phase}><span>{phase.state === "running" ? <LoaderCircle className="spin" size={11} /> : phase.state === "failed" ? <CircleAlert size={11} /> : <Check size={11} />}</span><strong>{phase.title}</strong><small>{phase.detail}</small></li>)}</ol>
  </section>;
}

function AssistantVideoResult({ artifact, project, onOpen }: { artifact: VideoArtifact; project: VideoProject; onOpen: () => void }) {
  const { save, saving } = useArtifactSaver();
  const isMaster = artifact.role === "master";
  const isPlayableVideo = artifact.playable && artifact.mime_type.startsWith("video/") && Boolean(artifact.url);
  const resultLabel = isMaster ? "Final video master" : artifact.role === "preview" ? "Video preview" : artifact.role.replaceAll("-", " ");
  const secondary = project.manifest.artifacts.filter((candidate) => candidate.id !== artifact.id && candidate.role !== "source");
  return <article className="assistant-video-master" aria-label={`${resultLabel}: ${artifact.title}`}>
    <div className="assistant-video-master-media">{isPlayableVideo ? <video aria-label={`Play ${artifact.title}`} controls playsInline preload={artifact.poster_url ?? project.poster_url ? "metadata" : "auto"} poster={artifact.poster_url ?? project.poster_url} src={videoSourceForIdlePoster(artifact.url, artifact.poster_url ?? project.poster_url)} /> : <div><FileVideo2 aria-hidden="true" size={22} /><span>{isMaster ? "Master" : resultLabel} stored locally</span></div>}</div>
    <div className="assistant-video-master-copy"><span className="section-label">{isMaster ? "Final video master" : `Playable ${resultLabel.toLowerCase()}`}</span><strong>{artifact.title}</strong><small>{formatVideoDuration(artifact.duration_ms ?? project.duration_ms)} · {artifact.width && artifact.height ? `${artifact.width}×${artifact.height}` : artifact.format.toUpperCase()} · {artifact.codec ?? "Local render"}</small><div><button type="button" onClick={onOpen}><Clapperboard aria-hidden="true" size={12} />Open project</button>{artifact.local_path ? <button type="button" disabled={saving} aria-label={`Save ${artifact.title}`} onClick={() => void save(artifact.local_path, artifact.download_name ?? `${project.id}-${artifact.role}.${artifact.format}`).catch(() => undefined)}><Download aria-hidden="true" size={12} />Save</button> : null}</div></div>
    {secondary.length ? <details className="assistant-video-secondary"><summary><span>Project assets</span><small>{secondary.length} secondary</small><ChevronDown aria-hidden="true" size={12} /></summary><div>{secondary.slice(-6).map((candidate) => <AssistantVideoArtifactRow artifact={candidate} key={candidate.id} />)}</div></details> : null}
  </article>;
}

function AssistantVideoArtifactRow({ artifact }: { artifact: VideoArtifact }) {
  const { save, saving } = useArtifactSaver();
  const label = artifact.role.replaceAll("-", " ");
  return <div className="assistant-video-secondary-row"><span><FileVideo2 aria-hidden="true" size={11} /></span><strong>{artifact.title}</strong><small>{label}</small>{artifact.local_path ? <button type="button" disabled={saving} aria-label={`Save secondary asset ${artifact.title}`} onClick={() => void save(artifact.local_path, artifact.download_name).catch(() => undefined)}><Download aria-hidden="true" size={11} /></button> : null}</div>;
}

function AudioArtifact({ item, onRevise }: { item: HistoryItem; onRevise: () => void }) {
  const [source, setSource] = useState<string>();
  const [playing, setPlaying] = useState(false);
  const [time, setTime] = useState(0);
  const [duration, setDuration] = useState(item.duration_seconds || 0);
  const audioRef = useRef<HTMLAudioElement>(null);
  useEffect(() => {
    if (!item.audio_path) return;
    let active = true;
    let objectUrl: string | undefined;
    loadGeneratedAudio(item.audio_path).then((url) => { if (active) { objectUrl = url; setSource(url); } });
    return () => { active = false; if (objectUrl?.startsWith("blob:")) URL.revokeObjectURL(objectUrl); };
  }, [item.audio_path]);
  async function toggle() {
    const audio = audioRef.current;
    if (!audio) return;
    if (audio.paused) await audio.play(); else audio.pause();
  }
  return <article className="assistant-artifact">
    <audio ref={audioRef} src={source} onPlay={() => setPlaying(true)} onPause={() => setPlaying(false)} onEnded={() => setPlaying(false)} onLoadedMetadata={(event) => setDuration(event.currentTarget.duration)} onTimeUpdate={(event) => setTime(event.currentTarget.currentTime)} />
    <button className="assistant-artifact-play" type="button" aria-label={playing ? "Pause generated audio" : "Play generated audio"} disabled={!source} onClick={() => void toggle()}>{playing ? <Pause size={14} /> : <Play size={14} />}</button>
    <div className="assistant-artifact-main"><strong>{item.title || (item.generation_kind === "music" ? "Generated music" : "Generated speech")}</strong><span>{item.generation_kind === "music" ? "Music" : item.voice || "Voice"} · {formatTime(duration)}</span><input aria-label="Audio position" type="range" min={0} max={Math.max(duration, 0.01)} step={0.05} value={Math.min(time, duration)} onChange={(event) => { if (audioRef.current) audioRef.current.currentTime = Number(event.target.value); }} /></div>
    <div className="assistant-artifact-actions"><button type="button" onClick={onRevise}>Revise</button><button className="assistant-artifact-export" type="button" aria-label="Export generated audio" title="Export" onClick={() => void exportHistoryItem(item)}><Download size={14} /></button></div>
  </article>;
}

function humanize(value: string) { return value.replaceAll("_", " ").replaceAll("-", " ").replace(/\b\w/g, (letter) => letter.toUpperCase()); }
export function selectAssistantArtifacts(history: HistoryItem[], baseline: Set<string> | undefined, mode: "single" | "project") {
  return history
    .filter((item) => item.audio_path && !baseline?.has(item.id))
    .filter((item) => mode === "project" ? item.model_id === "soundar/project-master" || item.engine === "finishing" : item.model_id !== "soundar/project-master")
    .slice(0, 1);
}

export function selectAssistantJobs(jobs: JobRecord[], baseline: Set<string> | undefined) {
  return jobs
    .filter((job) => !baseline?.has(job.id))
    .filter((job) => ["queued", "preparing", "running"].includes(job.status))
    .filter((job) => ["synthesis", "api-synthesis", "music-generation"].includes(job.kind));
}

function AssistantJobProgress({ jobs, mode }: { jobs: JobRecord[]; mode: "single" | "project" }) {
  const job = jobs.find((item) => item.preview_audio_path) ?? jobs[0];
  const [source, setSource] = useState<string>();
  const [playing, setPlaying] = useState(false);
  const audioRef = useRef<HTMLAudioElement>(null);
  const progress = mode === "project"
    ? jobs.reduce((total, item) => total + item.progress, 0) / Math.max(1, jobs.length)
    : job.progress;
  useEffect(() => {
    if (mode === "project" || !job.preview_audio_path) {
      setSource(undefined);
      return;
    }
    let active = true;
    let objectUrl: string | undefined;
    loadJobPreview(job.id).then((url) => {
      objectUrl = url;
      if (active) setSource(url);
      else if (url.startsWith("blob:")) URL.revokeObjectURL(url);
    }).catch(() => undefined);
    return () => {
      active = false;
      if (objectUrl?.startsWith("blob:")) URL.revokeObjectURL(objectUrl);
    };
  }, [job.id, job.preview_audio_path, job.preview_duration_seconds, mode]);
  return <article className="assistant-job-progress" aria-live="polite">
    {source ? <audio ref={audioRef} src={source} onPlay={() => setPlaying(true)} onPause={() => setPlaying(false)} onEnded={() => setPlaying(false)} /> : null}
    <div className="assistant-job-progress-heading">
      <span>{mode === "project" ? "Project rendering" : job.stage === "decoding" ? "Preview ready" : "Generating locally"}</span>
      <strong>{Math.round(progress * 100)}%</strong>
    </div>
    <div className="assistant-job-progress-track"><span style={{ width: `${Math.max(4, progress * 100)}%` }} /></div>
    <div className="assistant-job-progress-meta">
      <span>{mode === "project" ? `${jobs.length} ${jobs.length === 1 ? "chapter" : "chapters"} active` : job.title?.slice(0, 54) || "Audio generation"}</span>
      {source ? <button type="button" aria-label={playing ? "Pause progressive preview" : "Play progressive preview"} onClick={() => { const audio = audioRef.current; if (!audio) return; if (audio.paused) void audio.play(); else audio.pause(); }}>{playing ? <Pause size={12} /> : <Play size={12} />}<span>{formatTime(job.preview_duration_seconds ?? 0)} preview</span></button> : <small>{humanize(job.stage ?? job.status)}</small>}
    </div>
  </article>;
}

function ActivitySummary({ runs }: { runs: ToolRun[] }) {
  const recent = runs.slice(-8);
  const running = recent.filter((run) => run.state === "running").length;
  const failed = recent.filter((run) => run.state === "failed").length;
  const label = failed ? "Action needs attention" : running ? `Working in soundAr · ${running} active` : `Activity complete · ${recent.length} actions`;
  return <details className={`assistant-activity-summary${failed ? " is-failed" : ""}`} open={Boolean(running || failed)}>
    <summary>{running ? <LoaderCircle className="spin" size={13} /> : failed ? <CircleAlert size={13} /> : <Check size={13} />}<span>{label}</span><ChevronDown size={13} /></summary>
    <div>{recent.map((run) => <div className={`assistant-activity-row is-${run.state}`} key={run.id}><span>{run.state === "running" ? <LoaderCircle className="spin" size={12} /> : run.state === "failed" ? <CircleAlert size={12} /> : <Check size={12} />}</span><strong>{run.title}</strong><small>{run.detail}</small></div>)}</div>
  </details>;
}

const videoToolPhases: Record<string, VideoJobPhase> = {
  preview_link: "source",
  preview_video_link: "source",
  import_link: "source",
  import_video_link: "source",
  import_video_file: "source",
  create_video_project: "review",
  analyze_video: "analyze",
  plan_video: "review",
  revise_video: "review",
  render_video_preview: "preview",
  export_video: "export",
  export_publish_package: "export",
};

const readOnlyVideoTools = new Set(["list_video_projects", "get_video_project", "video_runtime_status", "cancel_video_job", "resume_video_job"]);
const videoPhaseOrder: VideoJobPhase[] = ["source", "analyze", "review", "preview", "export"];

function normalizeToolName(tool: string) {
  const normalized = tool.toLowerCase();
  const known = [...Object.keys(videoToolPhases), ...readOnlyVideoTools];
  return known.find((name) => normalized === name || normalized.endsWith(`__${name}`) || normalized.endsWith(`/${name}`) || normalized.endsWith(`:${name}`) || normalized.endsWith(`.${name}`))
    ?? normalized.split(/[.:/]/).at(-1)
    ?? normalized;
}

export function videoPhaseForTool(tool: string): VideoJobPhase | undefined {
  return videoToolPhases[normalizeToolName(tool)];
}

export function isVideoTool(tool: string) {
  const normalized = normalizeToolName(tool);
  return normalized in videoToolPhases || readOnlyVideoTools.has(normalized);
}

function videoPhaseTitle(phase: VideoJobPhase) {
  return ({ source: "Source", analyze: "Analyze", review: "Plan & revise", preview: "Preview", export: "Export" } satisfies Record<VideoJobPhase, string>)[phase];
}

function upsertVideoPhase(phases: VideoPhaseRun[], next: VideoPhaseRun) {
  return [...phases.filter((phase) => phase.phase !== next.phase), next].sort((left, right) => videoPhaseOrder.indexOf(left.phase) - videoPhaseOrder.indexOf(right.phase));
}

function parseStructuredValue(value: unknown): unknown {
  if (typeof value !== "string") return value;
  const trimmed = value.trim();
  if (!trimmed.startsWith("{") && !trimmed.startsWith("[")) return value;
  try { return JSON.parse(trimmed); } catch { return value; }
}

function findStructuredValue<T>(root: unknown, predicate: (value: Record<string, unknown>) => T | undefined, depth = 0, visited = new WeakSet<object>()): T | undefined {
  const parsed = parseStructuredValue(root);
  if (depth > 6 || !parsed || typeof parsed !== "object") return undefined;
  if (visited.has(parsed)) return undefined;
  visited.add(parsed);
  if (Array.isArray(parsed)) {
    for (const value of parsed) {
      const found = findStructuredValue(value, predicate, depth + 1, visited);
      if (found !== undefined) return found;
    }
    return undefined;
  }
  const record = parsed as Record<string, unknown>;
  const direct = predicate(record);
  if (direct !== undefined) return direct;
  for (const value of Object.values(record)) {
    const found = findStructuredValue(value, predicate, depth + 1, visited);
    if (found !== undefined) return found;
  }
  return undefined;
}

export function videoProjectIdFromToolResult(item: Record<string, unknown>): string | undefined {
  return findStructuredValue(item, (value) => {
    if (typeof value.project_id === "string") return value.project_id;
    if (typeof value.projectId === "string") return value.projectId;
    if (typeof value.id === "string" && typeof value.name === "string" && value.manifest && typeof value.manifest === "object") return value.id;
    return undefined;
  });
}

export function videoProjectFromToolResult(item: Record<string, unknown>): VideoProject | undefined {
  return findStructuredValue(item, (value) => typeof value.id === "string" && typeof value.name === "string" && value.manifest && typeof value.manifest === "object" ? value as unknown as VideoProject : undefined);
}

function preferredVideoResult(project: VideoProject, phase?: VideoJobPhase): VideoArtifact | undefined {
  const artifacts = [...(project.deliverables ?? []), ...project.manifest.artifacts];
  const role = phase === "preview" ? "preview" : phase === "export" ? "master" : undefined;
  return [...artifacts].reverse().find((artifact) => (!role || artifact.role === role) && Boolean(artifact.url));
}

export function restoredVideoPhases(project: VideoProject): VideoPhaseRun[] {
  const phases: VideoPhaseRun[] = [];
  const add = (phase: VideoJobPhase, detail: string) => phases.push({
    phase,
    title: videoPhaseTitle(phase),
    detail,
    state: "complete",
  });
  add("source", "Saved source restored");
  if (project.manifest.transcript.length || project.manifest.candidates.length || project.manifest.scenes.length) add("analyze", "Saved analysis restored");
  if (project.manifest.scenes.length) add("review", "Reviewed timeline restored");
  if (project.manifest.artifacts.some((artifact) => artifact.role === "preview") || project.master) add("preview", "Playable preview restored");
  if (project.master) add("export", "Final master restored");
  return phases;
}

function formatVideoDuration(milliseconds: number) {
  const seconds = Math.max(0, Math.round(milliseconds / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

function previewCreativeResponse(prompt: string): { message: string; plan: PlanStep[]; tools: ToolRun[]; video?: boolean } {
  const normalized = prompt.toLowerCase();
  const isVideo = /video|reel|clip|caption|youtube|animated podcast/.test(normalized);
  const isSpeech = /speech|voice|podcast|narrat|audiobook|opening|spoken/.test(normalized);
  const isProject = /project|episode|chapter|course|campaign|series|audiobook/.test(normalized);
  if (isVideo) return {
    video: true,
    message: "I’ve shaped that into a local video production, kept the source timing intact, reviewed the scene plan, and prepared a playable portrait master. The assembled result is below; tell me what to shorten, restyle, or rerender and I’ll update only the affected stages.",
    plan: [
      { step: "Confirm the source and production brief", status: "completed" },
      { step: "Analyze, plan, and review the scenes", status: "completed" },
      { step: "Render the preview and final master", status: "completed" },
    ],
    tools: [],
  };
  const output = isSpeech ? "opening audio" : "music draft";
  return {
    message: `I’ve turned that into a working brief. I’ll research the context that affects the creative direction, draft the ${isSpeech ? "spoken content" : "music direction"}, ${isProject ? "organize it as a reusable project, " : ""}choose an installed local setup, and create a first ${output}. When it appears here, play it and tell me what to change—I’ll revise the same idea rather than make you start over.`,
    plan: [
      { step: "Shape the goal into a production brief", status: "completed" },
      { step: isSpeech ? "Draft the script and select a local voice" : "Draft the direction and select a local music model", status: "completed" },
      { step: `Generate and review the ${output}`, status: "completed" },
    ],
    tools: [
      { id: "preview-state", title: "Get studio state", detail: "Finished in soundAr", state: "complete" },
      { id: "preview-create", title: isSpeech ? "Queue speech generation" : "Queue music generation", detail: "Finished in soundAr", state: "complete" },
    ],
  };
}
function effortDescription(value: string) { return ({ none: "Answers without extended reasoning", minimal: "Fastest responses", low: "Quick tasks", medium: "Balanced planning", high: "Deeper planning and execution", xhigh: "Most thorough reasoning", max: "Maximum reasoning depth", ultra: "Extended maximum reasoning" } as Record<string, string>)[value] ?? "Reasoning level"; }
function sandboxPolicy(access: AgentAccess) {
  if (access === "danger-full-access") return { type: "dangerFullAccess" };
  if (access === "read-only") return { type: "readOnly" };
  return { type: "workspaceWrite", writableRoots: [], networkAccess: true, excludeTmpdirEnvVar: false, excludeSlashTmp: false };
}
function formatTime(seconds: number) { const safe = Number.isFinite(seconds) ? Math.max(0, seconds) : 0; return `${Math.floor(safe / 60)}:${Math.floor(safe % 60).toString().padStart(2, "0")}`; }
function readThreadMessages(thread: Record<string, unknown>): Message[] {
  const output: Message[] = [];
  const turns = Array.isArray(thread.turns) ? thread.turns : [];
  for (const turn of turns) {
    const items = typeof turn === "object" && turn && Array.isArray((turn as Record<string, unknown>).items) ? (turn as Record<string, unknown>).items as Array<Record<string, unknown>> : [];
    for (const item of items) {
      if (item.type === "agentMessage" && typeof item.text === "string") output.push({ id: String(item.id), role: "assistant", text: item.text });
      if (item.type === "userMessage" && Array.isArray(item.content)) {
        const text = item.content.map((content) => typeof content === "object" && content && "text" in content ? String((content as Record<string, unknown>).text) : "").join("\n").trim();
        if (text) output.push({ id: String(item.id), role: "user", text });
      }
    }
  }
  return output.length ? output : defaultMessages;
}

declare global { interface Window { __TAURI_INTERNALS__?: unknown } }
