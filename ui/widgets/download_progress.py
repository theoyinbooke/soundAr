"""Download progress widget — light theme."""
from __future__ import annotations

from PyQt6.QtWidgets import QLabel, QProgressBar, QVBoxLayout, QWidget

from ui.theme import COLORS


class DownloadProgressWidget(QWidget):
    def __init__(self, parent: QWidget | None = None) -> None:
        super().__init__(parent)
        self.setFixedWidth(180)

        self.progress_bar = QProgressBar()
        self.progress_bar.setRange(0, 100)
        self.progress_bar.setValue(0)
        self.progress_bar.setTextVisible(False)

        self.label = QLabel("Downloading\u2026")
        self.label.setStyleSheet(
            f"font-size: 11px; color: {COLORS['text_tertiary']};"
        )

        layout = QVBoxLayout(self)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(6)
        layout.addWidget(self.progress_bar)
        layout.addWidget(self.label)

    def set_progress(self, downloaded: float, total: float) -> None:
        percent = 0 if total <= 0 else int((downloaded / total) * 100)
        self.progress_bar.setValue(max(0, min(percent, 100)))
        if total > 0:
            dl_gb = downloaded / (1024 ** 3) if downloaded > 1000 else downloaded
            tot_gb = total / (1024 ** 3) if total > 1000 else total
            self.label.setText(f"{dl_gb:.1f} / {tot_gb:.1f} GB")
        else:
            self.label.setText(f"Downloading\u2026 {percent}%")

    def mark_complete(self) -> None:
        self.progress_bar.setValue(100)
        self.label.setText("Download complete")
        self.label.setStyleSheet(
            f"font-size: 11px; color: {COLORS['success']};"
        )
