import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { hasTauriRuntime } from "./bridge";

export interface CodexStatus {
  available: boolean;
  connected: boolean;
  path?: string;
  version?: string;
  studio_root?: string;
  message?: string;
}

export interface CodexModel {
  id: string;
  model: string;
  displayName: string;
  description: string;
  isDefault: boolean;
  hidden: boolean;
  defaultReasoningEffort: ReasoningEffort;
  supportedReasoningEfforts: Array<{ reasoningEffort: ReasoningEffort; description?: string }>;
}

export type ReasoningEffort = "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra";
export type AgentAccess = "read-only" | "workspace-write" | "danger-full-access";

export interface CodexEvent {
  id?: number;
  method: string;
  params?: Record<string, unknown>;
}

const previewModels: CodexModel[] = [
  { id: "gpt-5.6-sol", model: "gpt-5.6-sol", displayName: "GPT-5.6-Sol", description: "Latest frontier agentic model", isDefault: true, hidden: false, defaultReasoningEffort: "low", supportedReasoningEfforts: ["low", "medium", "high", "xhigh", "max", "ultra"].map((reasoningEffort) => ({ reasoningEffort: reasoningEffort as ReasoningEffort })) },
  { id: "gpt-5.6-terra", model: "gpt-5.6-terra", displayName: "GPT-5.6-Terra", description: "Balanced model for everyday studio work", isDefault: false, hidden: false, defaultReasoningEffort: "medium", supportedReasoningEfforts: ["low", "medium", "high", "xhigh", "max", "ultra"].map((reasoningEffort) => ({ reasoningEffort: reasoningEffort as ReasoningEffort })) },
  { id: "gpt-5.6-luna", model: "gpt-5.6-luna", displayName: "GPT-5.6-Luna", description: "Fast model for lightweight studio work", isDefault: false, hidden: false, defaultReasoningEffort: "medium", supportedReasoningEfforts: ["low", "medium", "high", "xhigh", "max"].map((reasoningEffort) => ({ reasoningEffort: reasoningEffort as ReasoningEffort })) },
  { id: "gpt-5.5", model: "gpt-5.5", displayName: "GPT-5.5", description: "Frontier model for complex work", isDefault: false, hidden: false, defaultReasoningEffort: "medium", supportedReasoningEfforts: ["low", "medium", "high", "xhigh"].map((reasoningEffort) => ({ reasoningEffort: reasoningEffort as ReasoningEffort })) },
];

export async function getCodexStatus(): Promise<CodexStatus> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return { available: true, connected: true, path: "/usr/local/bin/codex", version: "codex-cli preview", studio_root: "/home/studio/.soundAr" };
  return invoke("codex_agent_status");
}

export async function connectCodex(): Promise<CodexStatus> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return getCodexStatus();
  return invoke("codex_agent_connect");
}

export async function disconnectCodex(): Promise<boolean> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return true;
  return invoke("codex_agent_disconnect");
}

export async function codexRequest<T = unknown>(method: string, params: Record<string, unknown> = {}): Promise<T> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return previewRequest(method, params) as T;
  return invoke<T>("codex_agent_request", { method, params });
}

export async function respondToCodex(id: number, result: unknown): Promise<void> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return;
  await invoke("codex_agent_respond", { id, result });
}

export async function listenToCodex(handler: (event: CodexEvent) => void): Promise<UnlistenFn> {
  if (import.meta.env.DEV && !hasTauriRuntime()) return () => undefined;
  return listen<CodexEvent>("codex-agent-event", (event) => handler(event.payload));
}

export async function loadCodexModels(): Promise<CodexModel[]> {
  const response = await codexRequest<{ data: CodexModel[] }>("model/list", { includeHidden: false, limit: 50 });
  return response.data.filter((model) => !model.hidden);
}

function previewRequest(method: string, params: Record<string, unknown>): unknown {
  if (method === "account/read") return { requiresOpenaiAuth: true, account: { type: "chatgpt", email: "studio@example.com", planType: "pro" } };
  if (method === "model/list") return { data: previewModels, nextCursor: null };
  if (method === "thread/list") return { data: [], nextCursor: null };
  if (method === "thread/start") return { thread: { id: "preview-thread", turns: [], cwd: "/home/studio/.soundAr" }, model: previewModels[0].id };
  if (method === "turn/start") return { turn: { id: `preview-turn-${Date.now()}`, status: "completed", input: params.input } };
  if (method === "account/login/start") return { type: "chatgpt", loginId: "preview-login", authUrl: "https://chatgpt.com" };
  return {};
}
