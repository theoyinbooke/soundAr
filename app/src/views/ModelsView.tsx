import {
  AlertTriangle,
  Download,
  ExternalLink,
  HardDrive,
  LoaderCircle,
  Power,
  PowerOff,
  RefreshCw,
  Search,
  ShieldCheck,
  Trash2,
  X,
} from "lucide-react";
import { useDeferredValue, useEffect, useMemo, useState } from "react";
import { PageHeader, Panel, RowActionMenu, Segmented, StatusText } from "../components/ui";
import {
  cancelModelInstall,
  cancelJob,
  getModelInstallPlan,
  getEngineHealth,
  installModel,
  listJobs,
  queueModelRuntimeLoad,
  removeModel,
  setupEngineRuntime,
  unloadModelRuntime,
  verifyModel,
} from "../lib/bridge";
import type {
  BootstrapState,
  CatalogModel,
  ModelDownloadProgress,
  ModelInstallPlan,
  JobRecord,
} from "../types";

type Filter = "all" | "installed" | "available";
type ModalState =
  | { kind: "planning"; model: CatalogModel }
  | { kind: "install"; model: CatalogModel; plan: ModelInstallPlan }
  | { kind: "remove"; model: CatalogModel }
  | undefined;

function shortName(model: CatalogModel) {
  return model.model_id.split("/").at(-1)?.replaceAll("_", " ") ?? model.model_id;
}

function formatBytes(bytes?: number) {
  if (bytes === undefined || bytes <= 0) return "Calculated by provider";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const exponent = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  return `${(bytes / 1024 ** exponent).toFixed(exponent > 2 ? 1 : 0)} ${units[exponent]}`;
}

function compactRevision(revision?: string) {
  return revision ? revision.slice(0, 9) : "Legacy install";
}

function taskLabel(task: CatalogModel["task"]) {
  if (task === "speaker-verification") return "Speaker similarity";
  if (task === "alignment") return "Word alignment";
  return task.toUpperCase();
}

function engineLabel(engine: string) {
  if (engine === "speaker-verification") return "WavLM";
  if (engine === "alignment") return "Wav2Vec2";
  return engine;
}

export function ModelsView({ bootstrap, onChanged }: { bootstrap: BootstrapState; onChanged: () => Promise<void> }) {
  const [query, setQuery] = useState("");
  const deferredQuery = useDeferredValue(query.trim().toLowerCase());
  const [filter, setFilter] = useState<Filter>("all");
  const [selectedId, setSelectedId] = useState(bootstrap.installed[0]?.model_id ?? bootstrap.catalog[0]?.model_id ?? "");
  const [modal, setModal] = useState<ModalState>();
  const [approved, setApproved] = useState(false);
  const [installingId, setInstallingId] = useState<string>();
  const [progress, setProgress] = useState<ModelDownloadProgress>();
  const [message, setMessage] = useState<{ tone: "success" | "danger"; text: string }>();
  const [busy, setBusy] = useState(false);
  const [engineSetup, setEngineSetup] = useState<{ engine: string; message: string }>();
  const [health, setHealth] = useState<{ modelId: string; text: string; tone: "success" | "danger" }>();
  const [loadedModels, setLoadedModels] = useState<Set<string>>(
    () => new Set(bootstrap.engine_runtimes.flatMap((runtime) => runtime.loaded_models)),
  );
  const [runtimeBusy, setRuntimeBusy] = useState<string>();
  const [runtimeLoadJob, setRuntimeLoadJob] = useState<JobRecord | undefined>(
    () => bootstrap.jobs.find((job) => job.kind === "model-load" && ["queued", "preparing", "running"].includes(job.status)),
  );
  const [runtimeCancelling, setRuntimeCancelling] = useState(false);

  const installedById = useMemo(
    () => new Map(bootstrap.installed.map((model) => [model.model_id, model])),
    [bootstrap.installed],
  );

  const models = useMemo(() => bootstrap.catalog.filter((model) => {
    const installed = installedById.has(model.model_id);
    if (filter === "installed" && !installed) return false;
    if (filter === "available" && installed) return false;
    if (!deferredQuery) return true;
    return [model.model_id, model.engine, model.task, model.summary, ...model.languages]
      .join(" ")
      .toLowerCase()
      .includes(deferredQuery);
  }), [bootstrap.catalog, deferredQuery, filter, installedById]);

  const selected = bootstrap.catalog.find((model) => model.model_id === selectedId) ?? models[0];
  const selectedInstall = selected ? installedById.get(selected.model_id) : undefined;
  const selectedRuntime = selected ? bootstrap.engine_runtimes.find((runtime) => runtime.engine === selected.engine) : undefined;
  const progressPercent = progress?.total_bytes
    ? Math.min(100, Math.round((progress.downloaded_bytes / progress.total_bytes) * 100))
    : 0;

  useEffect(() => {
    if (!runtimeLoadJob || !["queued", "preparing", "running"].includes(runtimeLoadJob.status)) return;
    let stopped = false;
    const poll = async () => {
      try {
        const next = (await listJobs()).find((job) => job.id === runtimeLoadJob.id);
        if (stopped || !next) return;
        if (["queued", "preparing", "running"].includes(next.status)) {
          setRuntimeLoadJob(next);
          return;
        }
        if (next.status === "completed") {
          const model = bootstrap.catalog.find((entry) => entry.model_id === next.model_id);
          if (model) {
            const healthResult = await getEngineHealth(model.engine);
            if (stopped) return;
            setLoadedModels((current) => {
              const resident = new Set([...current].filter((id) => bootstrap.catalog.find((entry) => entry.model_id === id)?.engine !== model.engine));
              for (const loaded of healthResult.loaded_models ?? []) resident.add(loaded);
              return resident;
            });
            setMessage({ tone: "success", text: `${shortName(model)} is loaded on ${healthResult.device}.` });
            await onChanged();
          }
        } else if (next.status === "cancelled") {
          setMessage({ tone: "success", text: "Model loading was cancelled and its worker was released." });
        } else {
          setMessage({ tone: "danger", text: next.error || "The model could not be loaded." });
        }
        setRuntimeCancelling(false);
        if (!stopped) setRuntimeLoadJob(undefined);
      } catch (error) {
        if (!stopped) {
          setRuntimeCancelling(false);
          setRuntimeLoadJob(undefined);
          setMessage({ tone: "danger", text: error instanceof Error ? error.message : String(error) });
        }
      }
    };
    void poll();
    const timer = window.setInterval(() => void poll(), 400);
    return () => {
      stopped = true;
      window.clearInterval(timer);
    };
  }, [bootstrap.catalog, onChanged, runtimeLoadJob]);

  async function prepareInstall(model: CatalogModel) {
    setMessage(undefined);
    setApproved(false);
    setModal({ kind: "planning", model });
    try {
      const plan = await getModelInstallPlan(model.model_id, installedById.get(model.model_id)?.revision);
      setModal({ kind: "install", model, plan });
    } catch (error) {
      setModal(undefined);
      setMessage({ tone: "danger", text: error instanceof Error ? error.message : String(error) });
    }
  }

  async function beginInstall(model: CatalogModel, plan: ModelInstallPlan) {
    setModal(undefined);
    setInstallingId(model.model_id);
    setProgress({ model_id: model.model_id, downloaded_bytes: 0, total_bytes: plan.download_size_bytes });
    setMessage(undefined);
    try {
      await installModel(plan, setProgress);
      await onChanged();
      setMessage({ tone: "success", text: `${shortName(model)} was verified and is ready.` });
    } catch (error) {
      const text = error instanceof Error ? error.message : String(error);
      setMessage({ tone: "danger", text });
    } finally {
      setInstallingId(undefined);
      setProgress(undefined);
    }
  }

  async function cancelInstall() {
    if (!installingId) return;
    setBusy(true);
    try {
      await cancelModelInstall(installingId);
    } finally {
      setBusy(false);
    }
  }

  async function confirmRemove(model: CatalogModel) {
    setBusy(true);
    setMessage(undefined);
    try {
      await removeModel(model.model_id);
      await onChanged();
      setModal(undefined);
      setMessage({ tone: "success", text: `${shortName(model)} was removed. Your generated audio was kept.` });
    } catch (error) {
      setMessage({ tone: "danger", text: error instanceof Error ? error.message : String(error) });
    } finally {
      setBusy(false);
    }
  }

  async function prepareEngine(engine: string) {
    if (engineSetup) return;
    setEngineSetup({ engine, message: `Preparing isolated ${engine} runtime...` });
    setMessage(undefined);
    try {
      await setupEngineRuntime(engine, (message) => setEngineSetup({ engine, message }));
      await onChanged();
      setMessage({ tone: "success", text: `${engine} now runs in its own pinned dependency layer.` });
    } catch (error) {
      setMessage({ tone: "danger", text: error instanceof Error ? error.message : String(error) });
    } finally {
      setEngineSetup(undefined);
    }
  }

  async function checkHealth(model: CatalogModel) {
    setHealth(undefined);
    try {
      const [result, integrity] = await Promise.all([
        getEngineHealth(model.engine),
        verifyModel(model.model_id),
      ]);
      await onChanged();
      const workers = `${result.warm_workers} warm / ${result.worker_starts} starts`;
      setLoadedModels((current) => {
        const next = new Set(current);
        for (const candidate of bootstrap.catalog.filter((entry) => entry.engine === model.engine)) next.delete(candidate.model_id);
        for (const loaded of result.loaded_models ?? []) next.add(loaded);
        return next;
      });
      const recovery = result.worker_failures
        ? ` / ${result.worker_failures} failures / ${result.worker_restarts} recovered`
        : " / no failures";
      const integrityText = integrity.state === "ready"
        ? `files ready${integrity.manifest_verified ? " / pinned manifest verified" : " / legacy structural check"}`
        : integrity.state === "not-installed"
          ? "not installed"
          : `repair needed / ${[...integrity.missing_files, ...integrity.invalid_files].slice(0, 2).join(", ") || integrity.reason}`;
      const residency = result.loaded_models?.length ? ` / ${result.loaded_models.map(shortModelId).join(", ")} loaded` : " / no model loaded";
      setHealth({ modelId: model.model_id, text: `${integrityText} / ${result.status} on ${result.device} / ${result.engine_runtime} / ${workers}${residency}${recovery}`, tone: integrity.state === "repair-needed" ? "danger" : "success" });
    } catch (error) {
      setHealth({ modelId: model.model_id, text: error instanceof Error ? error.message : String(error), tone: "danger" });
    }
  }

  async function changeResidency(model: CatalogModel, action: "load" | "unload") {
    if (runtimeBusy) return;
    setRuntimeBusy(model.model_id);
    setMessage(undefined);
    try {
      if (action === "load") {
        const job = await queueModelRuntimeLoad(model.model_id);
        setRuntimeLoadJob(job);
        setMessage({ tone: "success", text: `${shortName(model)} is loading through the GPU scheduler.` });
      } else {
        const result = await unloadModelRuntime(model.model_id);
        setLoadedModels((current) => new Set([...current].filter((id) => bootstrap.catalog.find((entry) => entry.model_id === id)?.engine !== model.engine)));
        setMessage({ tone: "success", text: `${shortName(model)} was unloaded; ${result.retired_workers ?? 0} warm worker${result.retired_workers === 1 ? "" : "s"} released.` });
      }
    } catch (error) {
      setMessage({ tone: "danger", text: error instanceof Error ? error.message : String(error) });
    } finally {
      setRuntimeBusy(undefined);
    }
  }

  async function cancelRuntimeLoad() {
    if (!runtimeLoadJob) return;
    try {
      setRuntimeCancelling(true);
      await cancelJob(runtimeLoadJob.id);
      setMessage({ tone: "success", text: "Cancelling model load and releasing its worker..." });
    } catch (error) {
      setRuntimeCancelling(false);
      setMessage({ tone: "danger", text: error instanceof Error ? error.message : String(error) });
    }
  }

  return (
    <div className="page models-page">
      <PageHeader title="Models" subtitle="Choose and manage model weights stored only on this machine." />

      <div className="data-toolbar">
        <label className="search-control">
          <Search aria-hidden="true" size={14} />
          <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search models, engines, capabilities..." />
        </label>
        <Segmented
          label="Model filter"
          value={filter}
          onChange={setFilter}
          options={[
            { value: "all", label: "All" },
            { value: "installed", label: "Installed" },
            { value: "available", label: "Available" },
          ]}
        />
        <button className="button button-secondary" type="button" onClick={() => void onChanged()}>
          <RefreshCw aria-hidden="true" size={13} />
          Refresh
        </button>
      </div>

      {message ? <div className={`model-notice is-${message.tone}`} role="status">{message.text}</div> : null}
      {engineSetup ? <div className="model-download" aria-live="polite"><div className="model-download-copy"><LoaderCircle className="spin" aria-hidden="true" size={15} /><span><strong>{engineSetup.engine} runtime</strong>{engineSetup.message}</span></div></div> : null}
      {installingId ? (
        <div className="model-download" aria-live="polite">
          <div className="model-download-copy">
            <LoaderCircle className="spin" aria-hidden="true" size={15} />
            <span><strong>{installingId}</strong>{formatBytes(progress?.downloaded_bytes)} / {formatBytes(progress?.total_bytes)}</span>
          </div>
          <div className="model-progress" aria-label={`Download ${progressPercent}%`}><i style={{ width: `${progressPercent}%` }} /></div>
          <span className="mono-cell">{progressPercent}%</span>
          <button className="button button-secondary" type="button" disabled={busy} onClick={() => void cancelInstall()}>
            <X aria-hidden="true" size={13} /> Cancel
          </button>
        </div>
      ) : null}

      <Panel className="table-panel model-table-panel" ariaLabel="Model registry">
        <div className="table-scroll">
          <table className="data-table model-table">
            <thead>
              <tr>
                <th>Model</th>
                <th>Engine</th>
                <th>Languages</th>
                <th>Hardware</th>
                <th>Status</th>
                <th aria-label="Actions" />
              </tr>
            </thead>
            <tbody>
              {models.map((model) => {
                const installed = installedById.get(model.model_id);
                const repairNeeded = installed?.integrity?.state === "repair-needed";
                const isInstalling = installingId === model.model_id;
                const installable = model.install_status === "ready";
                return (
                  <tr className={selectedId === model.model_id ? "is-selected" : ""} key={model.model_id} onClick={() => setSelectedId(model.model_id)}>
                    <td><strong>{shortName(model)}</strong><small>{model.model_id}</small></td>
                    <td><span className="engine-cell" title={model.engine}>{engineLabel(model.engine)}</span></td>
                    <td>{model.languages.length === 1 ? model.languages[0] : `${model.languages.length} languages`}</td>
                    <td>{model.recommended_for_12gb ? "12 GB ready" : "Advanced"}</td>
                    <td>
                      <StatusText tone={repairNeeded ? "danger" : installed ? "success" : installable ? "warning" : "muted"}>
                        {repairNeeded ? "Repair needed" : installed ? "Installed" : installable ? "Available" : "Planned"}
                      </StatusText>
                    </td>
                    <td>
                      <div className="table-row-actions">
                        <RowActionMenu
                          label={`More actions for ${shortName(model)}`}
                          actions={[
                            {
                              label: "View details",
                              icon: <HardDrive aria-hidden="true" size={12} />,
                              onSelect: () => setSelectedId(model.model_id),
                            },
                            ...(repairNeeded || (!installed && installable) ? [{
                              label: repairNeeded ? "Repair model" : isInstalling ? "Installing model" : "Install model",
                              icon: <Download aria-hidden="true" size={12} />,
                              disabled: Boolean(installingId),
                              onSelect: () => prepareInstall(model),
                            }] : []),
                            {
                              label: "Open model source",
                              icon: <ExternalLink aria-hidden="true" size={12} />,
                              disabled: !model.source_urls?.[0],
                              onSelect: () => { window.open(model.source_urls?.[0], "_blank", "noopener,noreferrer"); },
                            },
                          ]}
                        />
                      </div>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>

        {selected ? (
          <div className="model-inspector">
            <div className="model-inspector-main">
              <span className="section-label">Selected</span>
              <strong>{shortName(selected)}</strong>
              <p>{selected.summary}</p>
            </div>
            <dl className="model-inspector-facts">
              <div><dt>Task</dt><dd>{taskLabel(selected.task)}</dd></div>
              <div><dt>Revision</dt><dd className="mono-cell">{compactRevision(selectedInstall?.revision)}</dd></div>
              <div><dt>Local size</dt><dd>{selectedInstall ? formatBytes(selectedInstall.installed_size_bytes) : "Not installed"}</dd></div>
              <div><dt>License</dt><dd>{selectedInstall?.license ?? "Shown before install"}</dd></div>
              <div><dt>Runtime</dt><dd><StatusText tone={selectedRuntime?.state === "layered" ? "success" : "warning"}>{selectedRuntime?.state === "layered" ? "Pinned layer" : "Legacy shared"}</StatusText></dd></div>
              {selectedInstall?.integrity ? <div><dt>Files</dt><dd><StatusText tone={selectedInstall.integrity.state === "ready" ? "success" : "danger"}>{selectedInstall.integrity.state === "ready" ? "Ready" : "Repair needed"}</StatusText></dd></div> : null}
            </dl>
            <div className="inspector-actions">
              <button className="button button-secondary" type="button" onClick={() => window.open(selected.source_urls?.[0], "_blank", "noopener,noreferrer")}>
                <ExternalLink aria-hidden="true" size={13} /> Source
              </button>
              <button className="button button-secondary" type="button" disabled={Boolean(engineSetup)} onClick={() => void prepareEngine(selected.engine)}>
                {engineSetup?.engine === selected.engine ? <LoaderCircle className="spin" aria-hidden="true" size={13} /> : <ShieldCheck aria-hidden="true" size={13} />}
                {selectedRuntime?.state === "layered" ? "Refresh runtime" : "Create runtime"}
              </button>
              <button className="button button-secondary" type="button" onClick={() => void checkHealth(selected)}><RefreshCw aria-hidden="true" size={13} /> Health</button>
              {selectedInstall ? runtimeLoadJob?.model_id === selected.model_id && ["queued", "preparing", "running"].includes(runtimeLoadJob.status) ? (
                <button className="button button-secondary" type="button" disabled={runtimeCancelling} onClick={() => void cancelRuntimeLoad()}><X aria-hidden="true" size={13} />{runtimeCancelling ? "Cancelling" : "Cancel load"}</button>
              ) : loadedModels.has(selected.model_id) ? (
                <button className="button button-secondary" type="button" disabled={bootstrap.runtime === "browser" || Boolean(runtimeBusy)} title={bootstrap.runtime === "browser" ? "Runtime residency requires the desktop app" : "Release this engine's warm workers and GPU memory"} onClick={() => void changeResidency(selected, "unload")}><PowerOff aria-hidden="true" size={13} />{runtimeBusy === selected.model_id ? "Unloading" : "Unload model"}</button>
              ) : (
                <button className="button button-secondary" type="button" disabled={bootstrap.runtime === "browser" || Boolean(runtimeBusy) || selectedInstall.integrity?.state !== "ready"} title={bootstrap.runtime === "browser" ? "Runtime residency requires the desktop app" : "Prewarm this verified model through the GPU scheduler"} onClick={() => void changeResidency(selected, "load")}><Power aria-hidden="true" size={13} />{runtimeBusy === selected.model_id ? "Loading" : "Load model"}</button>
              ) : null}
              {selectedInstall?.integrity?.state === "repair-needed" ? (
                <button className="button button-primary" type="button" disabled={Boolean(installingId)} onClick={() => void prepareInstall(selected)}><Download aria-hidden="true" size={13} /> Repair</button>
              ) : null}
              {selectedInstall ? (
                <button className="icon-button danger-button" type="button" title="Remove model" onClick={() => setModal({ kind: "remove", model: selected })}>
                  <Trash2 aria-hidden="true" size={14} />
                </button>
              ) : null}
            </div>
            {health?.modelId === selected.model_id ? <StatusText tone={health.tone}>{health.text}</StatusText> : null}
          </div>
        ) : null}
      </Panel>

      {modal ? (
        <div className="modal-backdrop" role="presentation" onMouseDown={(event) => { if (event.target === event.currentTarget && modal.kind !== "planning") setModal(undefined); }}>
          <section className="modal model-install-modal" role="dialog" aria-modal="true" aria-labelledby="model-modal-title">
            {modal.kind === "planning" ? (
              <div className="model-plan-loading">
                <LoaderCircle className="spin" aria-hidden="true" size={20} />
                <strong id="model-modal-title">Checking upstream details</strong>
                <span>Resolving the exact revision, license, and download size.</span>
              </div>
            ) : modal.kind === "install" ? (
              <>
                <header className="modal-header">
                  <div>
                    <h2 id="model-modal-title">{installedById.get(modal.model.model_id)?.integrity?.state === "repair-needed" ? "Repair" : "Install"} {shortName(modal.model)}</h2>
                    <p>{installedById.get(modal.model.model_id)?.integrity?.state === "repair-needed" ? "Missing or invalid files will be downloaded again." : "No model weights are included with soundAr."}</p>
                  </div>
                  <button className="icon-button" type="button" title="Close" onClick={() => setModal(undefined)}><X aria-hidden="true" size={14} /></button>
                </header>
                <div className="modal-body">
                  <div className="model-consent-lead"><ShieldCheck aria-hidden="true" size={17} /><span>This downloads directly from the listed provider and verifies the local files before use.</span></div>
                  <dl className="model-plan-facts">
                    <div><dt>Provider</dt><dd>Hugging Face</dd></div>
                    <div><dt>Download</dt><dd>{formatBytes(modal.plan.download_size_bytes)} / {modal.plan.file_count} files</dd></div>
                    <div><dt>License</dt><dd>{modal.plan.license}</dd></div>
                    <div><dt>Access</dt><dd>{modal.plan.access === "gated" ? "Provider approval required" : "Public"}</dd></div>
                    <div><dt>Revision</dt><dd className="mono-cell">{modal.plan.revision}</dd></div>
                    <div><dt>Location</dt><dd className="model-path">{modal.plan.model_cache_dir}</dd></div>
                  </dl>
                  {modal.model.known_limitations?.length ? (
                    <div className="model-limitations"><AlertTriangle aria-hidden="true" size={14} /><span>{modal.model.known_limitations[0]}</span></div>
                  ) : null}
                  <label className="model-approval">
                    <input type="checkbox" checked={approved} onChange={(event) => setApproved(event.target.checked)} />
                    <span>I reviewed the upstream license and approve this model download.</span>
                  </label>
                </div>
                <footer className="modal-actions">
                  <button className="button button-secondary" type="button" onClick={() => window.open(modal.plan.source_url, "_blank", "noopener,noreferrer")}><ExternalLink aria-hidden="true" size={13} /> Model card</button>
                  <button className="button button-primary" type="button" disabled={!approved} onClick={() => void beginInstall(modal.model, modal.plan)}><Download aria-hidden="true" size={13} /> {installedById.get(modal.model.model_id)?.integrity?.state === "repair-needed" ? "Download and repair" : "Download and install"}</button>
                </footer>
              </>
            ) : (
              <>
                <header className="modal-header"><div><h2 id="model-modal-title">Remove {shortName(modal.model)}</h2><p>Generated audio and projects will be kept.</p></div></header>
                <div className="modal-body"><div className="model-consent-lead"><HardDrive aria-hidden="true" size={17} /><span>The local model files and registry entry will be deleted. You can reinstall the model later.</span></div></div>
                <footer className="modal-actions">
                  <button className="button button-secondary" type="button" disabled={busy} onClick={() => setModal(undefined)}>Keep model</button>
                  <button className="button button-danger" type="button" disabled={busy} onClick={() => void confirmRemove(modal.model)}>{busy ? <LoaderCircle className="spin" aria-hidden="true" size={13} /> : <Trash2 aria-hidden="true" size={13} />} Remove files</button>
                </footer>
              </>
            )}
          </section>
        </div>
      ) : null}
    </div>
  );
}

function shortModelId(modelId: string) {
  return modelId.split("/").at(-1) ?? modelId;
}
