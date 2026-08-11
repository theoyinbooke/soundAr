"""Export dialog for comparison results — JSON/CSV format."""
from __future__ import annotations

import csv
import io
import json
from pathlib import Path
from typing import Any

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import (
    QComboBox,
    QDialog,
    QFileDialog,
    QHBoxLayout,
    QLabel,
    QPlainTextEdit,
    QPushButton,
    QVBoxLayout,
)


class ExportDialog(QDialog):
    """Dialog for exporting comparison results to JSON or CSV."""

    def __init__(
        self,
        results: list[dict[str, Any]],
        parent=None,
    ) -> None:
        super().__init__(parent)
        self._results = results
        self.setWindowTitle("Export Results")
        self.setMinimumSize(500, 400)
        self._build_ui()
        self._update_preview()

    def _build_ui(self) -> None:
        layout = QVBoxLayout(self)
        layout.setSpacing(12)

        # Format selector
        fmt_row = QHBoxLayout()
        fmt_row.addWidget(QLabel("Format:"))
        self._format_combo = QComboBox()
        self._format_combo.addItems(["JSON", "CSV"])
        self._format_combo.currentIndexChanged.connect(self._update_preview)
        fmt_row.addWidget(self._format_combo)
        fmt_row.addStretch()
        layout.addLayout(fmt_row)

        # Preview
        layout.addWidget(QLabel("Preview:"))
        self._preview = QPlainTextEdit()
        self._preview.setReadOnly(True)
        layout.addWidget(self._preview, 1)

        # Buttons
        btn_row = QHBoxLayout()
        btn_row.addStretch()

        save_btn = QPushButton("Save")
        save_btn.setObjectName("primary")
        save_btn.clicked.connect(self._on_save)
        btn_row.addWidget(save_btn)

        close_btn = QPushButton("Close")
        close_btn.clicked.connect(self.close)
        btn_row.addWidget(close_btn)

        layout.addLayout(btn_row)

    def _update_preview(self) -> None:
        fmt = self._format_combo.currentText()
        if fmt == "JSON":
            self._preview.setPlainText(self._to_json())
        else:
            self._preview.setPlainText(self._to_csv())

    def _to_json(self) -> str:
        export_data = []
        for r in self._results:
            entry = {
                "model_id": r.get("model_id", ""),
                "engine": r.get("engine", ""),
                "error": r.get("error"),
            }
            metrics = r.get("metrics")
            if metrics is not None:
                entry["metrics"] = {
                    "inference_seconds": metrics.inference_seconds,
                    "audio_duration_seconds": metrics.audio_duration_seconds,
                    "rtf": metrics.rtf,
                    "vram_peak_mb": metrics.vram_peak_mb,
                    "device": metrics.device,
                }
            result = r.get("result")
            if result is not None and hasattr(result, "text"):
                entry["text"] = result.text
            export_data.append(entry)
        return json.dumps(export_data, indent=2)

    def _to_csv(self) -> str:
        output = io.StringIO()
        writer = csv.writer(output)
        writer.writerow([
            "model_id", "engine", "inference_s", "audio_duration_s",
            "rtf", "vram_peak_mb", "device", "error", "text"
        ])
        for r in self._results:
            metrics = r.get("metrics")
            result = r.get("result")
            text = result.text if result and hasattr(result, "text") else ""
            writer.writerow([
                r.get("model_id", ""),
                r.get("engine", ""),
                f"{metrics.inference_seconds:.3f}" if metrics else "",
                f"{metrics.audio_duration_seconds:.1f}" if metrics else "",
                f"{metrics.rtf:.4f}" if metrics else "",
                f"{metrics.vram_peak_mb:.0f}" if metrics else "",
                metrics.device if metrics else "",
                r.get("error", ""),
                text,
            ])
        return output.getvalue()

    def _on_save(self) -> None:
        fmt = self._format_combo.currentText()
        ext = "json" if fmt == "JSON" else "csv"
        filter_str = f"{fmt} files (*.{ext})"

        path, _ = QFileDialog.getSaveFileName(
            self, "Save export", f"comparison.{ext}", filter_str
        )
        if path:
            content = self._to_json() if fmt == "JSON" else self._to_csv()
            Path(path).write_text(content, encoding="utf-8")
            self.close()
