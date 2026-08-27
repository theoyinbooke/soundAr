import { ArrowUp, Bot, Check, ChevronDown, CircleAlert, CircleStop, Download, ExternalLink, LoaderCircle, MessageCircle, MoreHorizontal, PanelRightClose, Pause, Play, Plus, ShieldCheck, X } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { codexRequest, connectCodex, getCodexStatus, listenToCodex, loadCodexModels, respondToCodex, type AgentAccess, type CodexEvent, type CodexModel, type CodexStatus, type ReasoningEffort } from "../lib/codexBridge";
import { exportHistoryItem, listHistory, loadGeneratedAudio } from "../lib/bridge";
import type { HistoryItem } from "../types";

type Message = { id: string; role: "user" | "assistant" | "system"; text: string; pending?: boolean };
type ToolRun = { id: string; title: string; detail: string; state: "running" | "complete" | "failed" };
type Approval = { id: number; method: string; title: string; detail: string };
type ThreadSummary = { id: string; preview?: string; name?: string; updatedAt?: number };
type PlanStep = { step: string; status: "pending" | "inProgress" | "completed" };

const defaultMessages: Message[] = [{ id: "welcome", role: "assistant", text: "Tell me what you are building—even if the idea is unfinished. I can research and plan it, write the content, create speech or music, assemble projects and batches, and revise the result with you." }];

export function AssistantPane({ open, onClose, onStudioChanged }: { open: boolean; onClose: () => void; onStudioChanged?: () => void }) {
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
  const [connecting, setConnecting] = useState(false);
  const [sending, setSending] = useState(false);
  const [menu, setMenu] = useState<"model" | "effort" | "access">();
  const [threadMenuOpen, setThreadMenuOpen] = useState(false);
  const [threads, setThreads] = useState<ThreadSummary[]>([]);
  const [artifacts, setArtifacts] = useState<HistoryItem[]>([]);
  const scrollRef = useRef<HTMLDivElement>(null);
  const initialHistoryIds = useRef<Set<string> | undefined>(undefined);
  const artifactKey = useRef("");
  const composerRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    if (!open) return;
    let active = true;
    let unlisten: () => void = () => undefined;
    setConnecting(true);
    getCodexStatus()
      .then(async (current) => current.connected ? current : current.available ? connectCodex() : current)
      .then(async (current) => {
        if (!active) return;
        setStatus(current);
        if (!current.connected) return;
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
  }, [open]);

  useEffect(() => {
    if (!open) return;
    let active = true;
    let timer = 0;
    async function refreshArtifacts() {
      try {
        const history = await listHistory();
        if (!active) return;
        if (!initialHistoryIds.current) initialHistoryIds.current = new Set(history.map((item) => item.id));
        const created = history.filter((item) => item.audio_path && !initialHistoryIds.current?.has(item.id)).slice(0, 4);
        if (created.length) {
          const key = created.map((item) => item.id).join(":");
          if (key !== artifactKey.current) {
            artifactKey.current = key;
            setArtifacts(created);
            onStudioChanged?.();
          }
        }
      } finally {
        if (active) timer = window.setTimeout(refreshArtifacts, sending ? 1800 : 4500);
      }
    }
    void refreshArtifacts();
    return () => { active = false; window.clearTimeout(timer); };
  }, [open, sending, onStudioChanged]);

  useEffect(() => {
    const element = scrollRef.current;
    if (element && typeof element.scrollTo === "function") element.scrollTo({ top: element.scrollHeight, behavior: "smooth" });
  }, [messages, toolRuns, approval]);

  const selectedModel = useMemo(() => models.find((model) => model.id === modelId) ?? models[0], [modelId, models]);
  const efforts = selectedModel?.supportedReasoningEfforts?.map((option) => option.reasoningEffort) ?? ["low", "medium", "high"];

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
        const title = humanize(String(item.tool ?? "soundAr tool"));
        setToolRuns((runs) => [...runs.filter((run) => run.id !== id), { id, title, detail: event.method === "item/completed" ? "Finished in soundAr" : "Working in soundAr", state: event.method === "item/completed" ? "complete" : "running" }]);
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

  async function send() {
    const text = draft.trim();
    if (!text || sending || !status.connected || !account) return;
    setDraft("");
    setError(undefined);
    setSending(true);
    setPlanSteps([]);
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
      setThreadId(id);
      setMessages(readThreadMessages(response.thread));
      setToolRuns([]);
      setPlanSteps([]);
      setArtifacts([]);
    } catch (caught) { setError(caught instanceof Error ? caught.message : String(caught)); }
  }

  if (!open) return null;
  return <aside className="assistant-pane" aria-label="soundAr assistant">
    <header className="assistant-header">
      <div><strong>Assistant</strong><span>{account ? "Powered by your Codex login" : "Codex connection"}</span></div>
      <div className="assistant-header-actions"><button type="button" aria-label="New conversation" title="New conversation" onClick={() => { setThreadId(undefined); setMessages(defaultMessages); setToolRuns([]); setPlanSteps([]); setArtifacts([]); }}><Plus size={16} /></button><button type="button" aria-label="Conversation history" title="Conversation history" aria-expanded={threadMenuOpen} onClick={() => setThreadMenuOpen((value) => !value)}><MoreHorizontal size={16} /></button><button type="button" aria-label="Close assistant" title="Close assistant" onClick={onClose}><PanelRightClose size={16} /></button></div>
      {threadMenuOpen ? <div className="assistant-thread-menu" role="menu" aria-label="Assistant conversations"><strong>Recent conversations</strong>{threads.length ? threads.map((thread) => <button role="menuitem" type="button" key={thread.id} onClick={() => void resumeThread(thread.id)}><span>{thread.name || thread.preview || "Untitled conversation"}</span><small>{thread.updatedAt ? new Date(thread.updatedAt * 1000).toLocaleDateString() : "Saved by Codex"}</small></button>) : <span>No saved conversations yet.</span>}</div> : null}
    </header>
    <div className="assistant-thread" ref={scrollRef}>
      {!status.available ? <div className="assistant-banner is-warning"><CircleAlert size={17} /><div><strong>Codex CLI not detected</strong><p>{status.message}</p><button type="button" onClick={() => location.reload()}>Scan again</button></div></div> : null}
      {status.available && !status.connected && !connecting ? <div className="assistant-banner is-warning"><CircleAlert size={17} /><div><strong>Could not connect to Codex</strong><p>{error ?? "The detected Codex installation did not start."}</p><button type="button" onClick={() => location.reload()}>Reconnect</button></div></div> : null}
      {connecting ? <div className="assistant-loading"><LoaderCircle className="spin" size={18} /><span>Connecting to Codex…</span></div> : null}
      {status.connected && !account && !connecting ? <div className="assistant-signin"><Bot size={25} /><strong>Connect your ChatGPT account</strong><p>soundAr uses the login managed by your existing Codex installation. Your credentials stay with Codex.</p><button className="primary-button" type="button" onClick={() => void startLogin()}>Sign in with ChatGPT <ExternalLink size={13} /></button></div> : null}
      {account ? <>
        {messages.map((message) => <article className={`assistant-message is-${message.role}`} key={message.id}><span>{message.role === "assistant" ? "Assistant" : message.role === "user" ? "You" : "soundAr"}</span><p>{message.text}{message.pending ? <i className="assistant-caret" /> : null}</p></article>)}
        {planSteps.length ? <ol className="assistant-plan" aria-label="Current plan">{planSteps.map((step, index) => <li className={`is-${step.status}`} key={`${index}-${step.step}`}><span>{step.status === "completed" ? <Check size={12} /> : step.status === "inProgress" ? <LoaderCircle className="spin" size={12} /> : index + 1}</span><strong>{step.step}</strong></li>)}</ol> : null}
        {toolRuns.length ? <div className="assistant-tool-stack">{toolRuns.slice(-6).map((run) => <div className={`assistant-tool is-${run.state}`} key={run.id}>{run.state === "running" ? <LoaderCircle className="spin" size={15} /> : run.state === "complete" ? <Check size={15} /> : <CircleAlert size={15} />}<div><strong>{run.title}</strong><span>{run.detail}</span></div></div>)}</div> : null}
        {artifacts.map((item) => <AudioArtifact key={item.id} item={item} onRevise={() => {
          setDraft(`Revise “${item.title || (item.generation_kind === "music" ? "Generated music" : "Generated speech")}” (${item.id}): `);
          window.setTimeout(() => composerRef.current?.focus(), 0);
        }} />)}
        {approval ? <div className="assistant-approval"><ShieldCheck size={18} /><div><strong>{approval.title}</strong><p>{approval.detail}</p><div><button type="button" onClick={() => void answerApproval(false)}>Deny</button><button className="primary-button" type="button" onClick={() => void answerApproval(true)}>Allow once</button></div></div></div> : null}
      </> : null}
      {error && status.connected ? <div className="assistant-inline-error"><CircleAlert size={14} /><span>{error}</span><button type="button" aria-label="Dismiss error" onClick={() => setError(undefined)}><X size={13} /></button></div> : null}
    </div>
    <footer className="assistant-composer-wrap">
      <div className="assistant-composer">
        <textarea ref={composerRef} aria-label="Message soundAr assistant" placeholder={account ? "Ask soundAr to create anything" : "Connect Codex to begin"} value={draft} disabled={!account || sending} onChange={(event) => setDraft(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void send(); } }} />
        <div className="assistant-composer-bar">
          <button type="button" className="assistant-control" disabled={!models.length} onClick={() => setMenu(menu === "model" ? undefined : "model")}><span>{selectedModel?.displayName ?? "Model"}</span><ChevronDown size={12} /></button>
          <button type="button" className="assistant-control" disabled={!selectedModel} onClick={() => setMenu(menu === "effort" ? undefined : "effort")}><span>{humanize(effort)}</span><ChevronDown size={12} /></button>
          <button type="button" className="assistant-control is-access" onClick={() => setMenu(menu === "access" ? undefined : "access")}><ShieldCheck size={12} /><span>{access === "danger-full-access" ? "Full access" : access === "workspace-write" ? "Studio access" : "Read only"}</span><ChevronDown size={12} /></button>
          <button className="assistant-send" type="button" aria-label={sending ? "Stop response" : "Send message"} disabled={!sending && (!draft.trim() || !account)} onClick={() => sending ? void interrupt() : void send()}>{sending ? <CircleStop size={16} /> : <ArrowUp size={16} />}</button>
        </div>
        {menu === "model" ? <div className="assistant-picker picker-model" role="menu" aria-label="Model">{models.map((model) => <button type="button" role="menuitemradio" aria-checked={model.id === modelId} key={model.id} onClick={() => { setModelId(model.id); setEffort(model.defaultReasoningEffort); setMenu(undefined); }}><span><strong>{model.displayName}</strong><small>{model.description}</small></span>{model.id === modelId ? <Check size={14} /> : null}</button>)}</div> : null}
        {menu === "effort" ? <div className="assistant-picker picker-effort" role="menu" aria-label="Reasoning effort">{efforts.map((value) => <button type="button" role="menuitemradio" aria-checked={value === effort} key={value} onClick={() => { setEffort(value); setMenu(undefined); }}><span><strong>{humanize(value)}</strong><small>{effortDescription(value)}</small></span>{value === effort ? <Check size={14} /> : null}</button>)}</div> : null}
        {menu === "access" ? <div className="assistant-picker picker-access" role="menu" aria-label="Access level">{(["read-only", "workspace-write", "danger-full-access"] as AgentAccess[]).map((value) => <button type="button" role="menuitemradio" aria-checked={value === access} key={value} onClick={() => { setAccess(value); setMenu(undefined); }}><span><strong>{value === "danger-full-access" ? "Full access" : value === "workspace-write" ? "Studio access" : "Read only"}</strong><small>{value === "danger-full-access" ? "Can use the machine with approvals" : value === "workspace-write" ? "Can research and manage soundAr work" : "Can inspect and plan only"}</small></span>{value === access ? <Check size={14} /> : null}</button>)}</div> : null}
      </div>
      <span className="assistant-disclaimer">Codex can make mistakes. Review actions before approval.</span>
    </footer>
  </aside>;
}

export function AssistantLauncher({ onClick }: { onClick: () => void }) {
  return <button className="assistant-launcher" type="button" aria-label="Open soundAr assistant" title="Open assistant" onClick={onClick}><MessageCircle size={19} /></button>;
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
function previewCreativeResponse(prompt: string): { message: string; plan: PlanStep[]; tools: ToolRun[] } {
  const normalized = prompt.toLowerCase();
  const isSpeech = /speech|voice|podcast|narrat|audiobook|opening|spoken/.test(normalized);
  const isProject = /project|episode|chapter|course|campaign|series|audiobook/.test(normalized);
  const output = isSpeech ? "opening audio" : "music draft";
  return {
    message: `I’ve turned that into a working brief. I’ll research the context that affects the creative direction, draft the ${isSpeech ? "spoken content" : "music direction"}, ${isProject ? "organize it as a reusable project, " : ""}choose an installed local setup, and create a first ${output}. When it appears here, play it and tell me what to change—I’ll revise the same idea rather than make you start over.`,
    plan: [
      { step: "Shape the goal into a production brief", status: "completed" },
      { step: isSpeech ? "Draft the script and select a local voice" : "Draft the direction and select a local music model", status: "completed" },
      { step: `Generate and review the ${output}`, status: "inProgress" },
    ],
    tools: [
      { id: "preview-state", title: "Get studio state", detail: "Finished in soundAr", state: "complete" },
      { id: "preview-create", title: isSpeech ? "Queue speech generation" : "Queue music generation", detail: "Working in soundAr", state: "running" },
    ],
  };
}
function effortDescription(value: string) { return ({ none: "Answers without extended reasoning", minimal: "Fastest responses", low: "Quick tasks", medium: "Balanced planning", high: "Deeper planning and execution", xhigh: "Most thorough reasoning" } as Record<string, string>)[value] ?? "Reasoning level"; }
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
