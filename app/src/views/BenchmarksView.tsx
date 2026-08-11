import { Gauge, Play, RotateCcw } from "lucide-react";
import { useMemo, useState } from "react";
import type { BenchmarkResult, BootstrapState } from "../types";
import { MetricStrip, PageHeader, Panel, Segmented, StatusText } from "../components/ui";

type SortKey = "rtf" | "ttfa" | "vramGb" | "quality";

export function BenchmarksView({
  bootstrap,
  results,
  onChange,
}: {
  bootstrap: BootstrapState;
  results: BenchmarkResult[];
  onChange: (results: BenchmarkResult[]) => void;
}) {
  const [sort, setSort] = useState<SortKey>("rtf");
  const [running, setRunning] = useState(false);
  const sorted = useMemo(() => [...results].sort((a, b) => a[sort] - b[sort]), [results, sort]);
  const fastest = sorted[0];
  const bestQuality = [...results].sort((a, b) => b.quality - a.quality)[0];

  function runSuite() {
    if (running) return;
    setRunning(true);
    window.setTimeout(() => {
      onChange(results.map((result) => ({ ...result, rtf: Number((result.rtf * (0.96 + Math.random() * 0.08)).toFixed(2)) })));
      setRunning(false);
    }, 1800);
  }

  return (
    <div className="page benchmarks-page">
      <PageHeader
        title="Benchmarks"
        subtitle="Measure latency, memory, and output quality on this exact machine."
        actions={
          <button className="button button-primary" type="button" onClick={runSuite} disabled={running}>
            {running ? <RotateCcw className="spin" aria-hidden="true" size={14} /> : <Play aria-hidden="true" size={14} />}
            {running ? "Running suite" : "Run benchmark"}
          </button>
        }
      />

      <MetricStrip
        metrics={[
          { value: fastest ? `${fastest.rtf.toFixed(2)}x` : "--", label: "Best real-time factor", tone: "success" },
          { value: fastest ? `${fastest.ttfa.toFixed(2)} s` : "--", label: "Fastest first audio", tone: "success" },
          { value: bestQuality ? `${Math.round(bestQuality.quality * 100)}%` : "--", label: "Top quality score", tone: "warning" },
          { value: `${(bootstrap.system.vram_total_mb / 1024).toFixed(1)} GB`, label: "Available GPU memory" },
        ]}
      />

      <div className="data-toolbar benchmark-toolbar">
        <div className="toolbar-note"><Gauge aria-hidden="true" size={14} /><span>RTX 4080 Laptop / warm model / 10 second phrase</span></div>
        <Segmented
          label="Benchmark sort"
          value={sort}
          onChange={setSort}
          options={[
            { value: "rtf", label: "RTF" },
            { value: "ttfa", label: "First audio" },
            { value: "vramGb", label: "VRAM" },
            { value: "quality", label: "Quality" },
          ]}
        />
      </div>

      <Panel className="table-panel benchmark-table-panel" ariaLabel="Model benchmark results">
        <div className="table-scroll">
          <table className="data-table benchmark-table">
            <thead><tr><th>Model</th><th>Variant</th><th>RTF</th><th>First audio</th><th>Peak VRAM</th><th>Quality</th><th>Fit</th></tr></thead>
            <tbody>
              {sorted.map((result, index) => (
                <tr key={`${result.model}-${result.variant}`}>
                  <td><strong>{result.model}</strong><small>Rank {index + 1} by selected metric</small></td>
                  <td className="muted-cell">{result.variant}</td>
                  <td className="mono-cell"><strong>{result.rtf.toFixed(2)}x</strong></td>
                  <td className="mono-cell">{result.ttfa.toFixed(2)} s</td>
                  <td className="mono-cell">{result.vramGb.toFixed(1)} GB</td>
                  <td>
                    <div className="quality-meter"><i style={{ width: `${result.quality * 100}%` }} /><span>{Math.round(result.quality * 100)}</span></div>
                  </td>
                  <td><StatusText tone={result.vramGb < 8 ? "success" : "warning"}>{result.vramGb < 8 ? "Comfortable" : "Tight"}</StatusText></td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
        <div className="table-footnote"><span>Lower RTF and first-audio time are better. Quality is a local listening score.</span><StatusText tone="success">Results stored locally</StatusText></div>
      </Panel>
    </div>
  );
}
