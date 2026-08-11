import { ExternalLink, FileBox, RefreshCw, Search, Settings2, SlidersHorizontal } from "lucide-react";
import { useDeferredValue, useMemo, useState } from "react";
import type { BootstrapState, CatalogModel } from "../types";
import { PageHeader, Panel, Segmented, StatusText } from "../components/ui";

type Filter = "all" | "installed" | "available";

const modelFacts: Record<string, { variant: string; vram: string; license: string; capabilities: string }> = {
  kokoro: { variant: "Native fp16", vram: "1.1 GB", license: "Apache-2.0", capabilities: "54 voices / fast / offline" },
  chatterbox: { variant: "Turbo fp16", vram: "7.4 GB", license: "MIT", capabilities: "Expressive / clone / reference audio" },
  coqui: { variant: "XTTS fp16", vram: "4.6 GB", license: "CPML", capabilities: "Clone / multilingual / 24 kHz" },
  transformers: { variant: "Default fp16", vram: "3.2 GB", license: "Model card", capabilities: "Local inference / baseline" },
  nemo: { variant: "TDT fp16", vram: "5.4 GB", license: "CC-BY-4.0", capabilities: "Fast transcription / English" },
  voxtral: { variant: "Default fp16", vram: "9.5 GB", license: "Apache-2.0", capabilities: "Audio understanding / multilingual" },
  cohere: { variant: "Default bf16", vram: "8.0 GB", license: "Apache-2.0", capabilities: "Multilingual STT / conformer" },
};

function shortName(model: CatalogModel) {
  return model.model_id.split("/").at(-1)?.replaceAll("_", " ") ?? model.model_id;
}

export function ModelsView({ bootstrap }: { bootstrap: BootstrapState }) {
  const [query, setQuery] = useState("");
  const deferredQuery = useDeferredValue(query.trim().toLowerCase());
  const [filter, setFilter] = useState<Filter>("all");
  const [selectedId, setSelectedId] = useState(bootstrap.installed[0]?.model_id ?? bootstrap.catalog[0]?.model_id ?? "");
  const [queued, setQueued] = useState<Set<string>>(() => new Set());
  const installedIds = useMemo(() => new Set(bootstrap.installed.map((model) => model.model_id)), [bootstrap.installed]);

  const models = useMemo(() => {
    return bootstrap.catalog.filter((model) => {
      const installed = installedIds.has(model.model_id);
      if (filter === "installed" && !installed) return false;
      if (filter === "available" && installed) return false;
      if (!deferredQuery) return true;
      return [model.model_id, model.engine, model.task, model.summary, ...model.languages]
        .join(" ")
        .toLowerCase()
        .includes(deferredQuery);
    });
  }, [bootstrap.catalog, deferredQuery, filter, installedIds]);

  const selected = bootstrap.catalog.find((model) => model.model_id === selectedId) ?? models[0];
  const selectedFacts = selected ? modelFacts[selected.engine] ?? modelFacts.transformers : modelFacts.transformers;

  function queueInstall(modelId: string) {
    setQueued((current) => new Set(current).add(modelId));
    window.setTimeout(() => {
      setQueued((current) => {
        const next = new Set(current);
        next.delete(modelId);
        return next;
      });
    }, 2200);
  }

  return (
    <div className="page models-page">
      <PageHeader title="Models" subtitle="Install and compare engines by what they can actually do on this machine." />

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
        <button className="button button-secondary" type="button">
          <RefreshCw aria-hidden="true" size={13} />
          Refresh registry
        </button>
      </div>

      <Panel className="table-panel model-table-panel" ariaLabel="Model registry">
        <div className="table-scroll">
          <table className="data-table model-table">
            <thead>
              <tr>
                <th>Model</th>
                <th>Engine</th>
                <th>Capabilities</th>
                <th>VRAM</th>
                <th>License</th>
                <th>Status</th>
                <th aria-label="Actions" />
              </tr>
            </thead>
            <tbody>
              {models.map((model) => {
                const installed = installedIds.has(model.model_id);
                const isQueued = queued.has(model.model_id);
                const facts = modelFacts[model.engine] ?? modelFacts.transformers;
                return (
                  <tr className={selectedId === model.model_id ? "is-selected" : ""} key={model.model_id} onClick={() => setSelectedId(model.model_id)}>
                    <td>
                      <strong>{shortName(model)}</strong>
                      <small>{facts.variant} / {model.task.toUpperCase()}</small>
                    </td>
                    <td><span className="engine-cell">{model.engine}</span></td>
                    <td>{facts.capabilities}</td>
                    <td className={facts.vram.startsWith("9") ? "danger-value" : "mono-cell"}>{facts.vram}</td>
                    <td className="muted-cell">{facts.license}</td>
                    <td>
                      <StatusText tone={isQueued ? "warning" : installed ? "success" : "warning"}>
                        {isQueued ? "Queued" : installed ? "Installed" : model.access === "gated" ? "Gated" : "Available"}
                      </StatusText>
                    </td>
                    <td>
                      {installed ? (
                        <button className="table-action" type="button" onClick={(event) => { event.stopPropagation(); setSelectedId(model.model_id); }}>
                          <Settings2 aria-hidden="true" size={13} />
                          Configure
                        </button>
                      ) : (
                        <button className="table-action is-primary" type="button" disabled={isQueued} onClick={(event) => { event.stopPropagation(); queueInstall(model.model_id); }}>
                          {isQueued ? "Queued" : "Install"}
                        </button>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>

        {selected ? (
          <div className="table-inspector">
            <div className="inspector-title">
              <span className="section-label">Selected</span>
              <strong>{shortName(selected)} / {selectedFacts.variant}</strong>
            </div>
            <StatusText tone="success">{selected.task === "tts" ? "Voice generation" : "Transcription"}</StatusText>
            <span>{selected.languages.length} languages</span>
            <span>{selected.recommended_for_12gb ? "Recommended for this GPU" : "Advanced runtime"}</span>
            <span>{selectedFacts.vram} peak estimate</span>
            <div className="inspector-actions">
              <button className="button button-secondary" type="button">
                <FileBox aria-hidden="true" size={13} /> Files
              </button>
              <button className="button button-secondary" type="button">
                <SlidersHorizontal aria-hidden="true" size={13} /> Variants
              </button>
              <button
                className="icon-button"
                type="button"
                title="Open model source"
                onClick={() => window.open(selected.source_urls?.[0], "_blank", "noopener,noreferrer")}
              >
                <ExternalLink aria-hidden="true" size={14} />
              </button>
            </div>
          </div>
        ) : null}
      </Panel>
    </div>
  );
}
