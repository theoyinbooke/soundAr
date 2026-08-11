"""Hub tab — model browser with search, filters, and status footer (light theme)."""
from __future__ import annotations

from typing import Any

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import (
    QComboBox,
    QFrame,
    QHBoxLayout,
    QLabel,
    QLineEdit,
    QPushButton,
    QScrollArea,
    QVBoxLayout,
    QWidget,
)

from config.settings import AppSettings
from core.hub_browser import HubBrowser
from core.model_manager import ModelManager
from ui.dialogs.model_detail_dialog import ModelDetailDialog
from ui.dialogs.message_box import ask_confirmation, show_error
from ui.theme import COLORS
from ui.widgets.model_card import ModelCard
from workers.download_worker import DownloadWorker


class HubTab(QWidget):
    def __init__(
        self,
        hub_browser: HubBrowser,
        model_manager: ModelManager,
        settings: AppSettings,
        parent: QWidget | None = None,
    ) -> None:
        super().__init__(parent)
        self.hub_browser = hub_browser
        self.model_manager = model_manager
        self.settings = settings
        self._workers: dict[str, DownloadWorker] = {}  # model_id -> worker
        self._cards_by_model_id: dict[str, ModelCard] = {}
        self.setStyleSheet("background: transparent;")

        root_layout = QVBoxLayout(self)
        root_layout.setContentsMargins(0, 0, 0, 0)
        root_layout.setSpacing(0)

        # --- Search & filters row ---
        controls = QHBoxLayout()
        controls.setSpacing(12)

        self.search_input = QLineEdit(self)
        self.search_input.setPlaceholderText("Search curated models\u2026")
        self.search_input.setFixedHeight(38)

        self.task_filter = QComboBox(self)
        self.task_filter.addItem("All tasks", "all")
        self.task_filter.addItem("STT", "stt")
        self.task_filter.addItem("TTS", "tts")
        self.task_filter.setFixedHeight(38)
        self.task_filter.setMinimumWidth(120)
        self.task_filter.setCurrentIndex(self.task_filter.findData(settings.default_task_filter))

        self.sort_filter = QComboBox(self)
        self.sort_filter.addItem("Default", "default")
        self.sort_filter.addItem("Name A\u2013Z", "name_asc")
        self.sort_filter.addItem("Tier", "tier")
        self.sort_filter.setFixedHeight(38)
        self.sort_filter.setMinimumWidth(120)

        controls.addWidget(self.search_input, 1)
        controls.addWidget(self.task_filter)
        controls.addWidget(self.sort_filter)

        root_layout.addLayout(controls)
        root_layout.addSpacing(20)

        # --- Model list container ---
        self.list_frame = QFrame(self)
        self.list_frame.setStyleSheet(
            f"QFrame {{"
            f"  background-color: {COLORS['bg_raised']};"
            f"  border: 1px solid {COLORS['border_subtle']};"
            f"  border-radius: 12px;"
            f"}}"
        )
        list_frame_layout = QVBoxLayout(self.list_frame)
        list_frame_layout.setContentsMargins(0, 0, 0, 0)
        list_frame_layout.setSpacing(0)

        self.results_container = QWidget(self)
        self.results_container.setStyleSheet("background: transparent; border: none;")
        self.results_layout = QVBoxLayout(self.results_container)
        self.results_layout.setContentsMargins(0, 0, 0, 0)
        self.results_layout.setSpacing(0)
        self.results_layout.setAlignment(Qt.AlignmentFlag.AlignTop)

        self.scroll_area = QScrollArea(self)
        self.scroll_area.setWidgetResizable(True)
        self.scroll_area.setWidget(self.results_container)
        self.scroll_area.setStyleSheet("background: transparent; border: none;")

        list_frame_layout.addWidget(self.scroll_area)

        root_layout.addWidget(self.list_frame, 1)
        root_layout.addSpacing(16)

        # --- Status footer ---
        self.footer = QLabel("")
        self.footer.setStyleSheet(f"font-size: 11px; color: {COLORS['text_ghost']};")
        root_layout.addWidget(self.footer)

        # --- Signals ---
        self.search_input.textChanged.connect(self.refresh_results)
        self.task_filter.currentIndexChanged.connect(self.refresh_results)
        self.sort_filter.currentIndexChanged.connect(self.refresh_results)

        self.refresh_results()

    def refresh_results(self) -> None:
        while self.results_layout.count():
            item = self.results_layout.takeAt(0)
            widget = item.widget()
            if widget is not None:
                widget.deleteLater()

        self._cards_by_model_id.clear()

        task = str(self.task_filter.currentData())
        query = self.search_input.text()
        results = self.hub_browser.search_models(
            query=query,
            task=task,
            limit=self.settings.hub_results_limit,
        )

        if not results:
            empty_label = QLabel("No curated models match the current filters.")
            empty_label.setAlignment(Qt.AlignmentFlag.AlignCenter)
            empty_label.setStyleSheet(
                f"color: {COLORS['text_ghost']}; padding: 40px; border: none; font-size: 13px;"
            )
            self.results_layout.addWidget(empty_label)
            self._update_footer(0, 0)
            return

        installed_count = 0
        for i, entry in enumerate(results):
            model_id = str(entry.get("model_id"))
            is_dl = self.model_manager.is_downloaded(model_id)
            if is_dl:
                installed_count += 1

            # If this model is currently downloading, show progress state
            is_downloading = model_id in self._workers
            card = ModelCard(entry, is_downloaded=is_dl)
            if is_downloading:
                card.set_busy(True)

            card.download_requested.connect(self._start_download)
            card.details_requested.connect(self._show_details)
            card.cancel_requested.connect(self._cancel_download)
            card.delete_requested.connect(self._delete_model)
            self.results_layout.addWidget(card)
            self._cards_by_model_id[model_id] = card

            if i < len(results) - 1:
                divider = QFrame()
                divider.setFixedHeight(1)
                divider.setStyleSheet(
                    f"background-color: {COLORS['border_subtle']}; border: none;"
                )
                self.results_layout.addWidget(divider)

        self.results_layout.addStretch(1)

        self._update_footer(len(results), installed_count)

    def _update_footer(self, total: int, installed: int) -> None:
        self.footer.setText(
            f"{total} model{'s' if total != 1 else ''} \u00b7 "
            f"{installed} installed"
        )

    def _show_details(self, model_id: str) -> None:
        try:
            details = self.hub_browser.get_model_details(model_id)
        except Exception as exc:
            show_error(self, "Model details unavailable", str(exc))
            return

        dialog = ModelDetailDialog(details, self)
        dialog.exec()

    # ── Downloads ───────────────────────────────────────────

    def _start_download(self, model_id: str) -> None:
        if model_id in self._workers:
            return  # already downloading

        card = self._cards_by_model_id.get(model_id)
        if card is not None:
            card.set_busy(True)

        worker = DownloadWorker(model_id=model_id, model_manager=self.model_manager)
        worker.progress.connect(lambda dl, tot, mid=model_id: self._on_download_progress(mid, dl, tot))
        worker.finished.connect(lambda path, mid=model_id: self._on_download_finished(mid, path))
        worker.error.connect(lambda message, mid=model_id: self._on_download_error(mid, message))
        worker.cancelled.connect(lambda mid=model_id: self._on_download_cancelled(mid))
        self._workers[model_id] = worker
        worker.start()

    def _cancel_download(self, model_id: str) -> None:
        worker = self._workers.get(model_id)
        if worker is not None:
            worker.cancel()

    def _delete_model(self, model_id: str) -> None:
        if not ask_confirmation(
            self,
            "Delete model",
            f"Remove the local files for {model_id}?",
        ):
            return

        removed = self.model_manager.delete_model(model_id)
        if not removed and not self.model_manager.is_downloaded(model_id):
            self.refresh_results()
            return
        self.refresh_results()

    def _on_download_progress(self, model_id: str, downloaded: float, total: float) -> None:
        card = self._cards_by_model_id.get(model_id)
        if card is not None:
            card.update_progress(downloaded, total)

    def _on_download_finished(self, model_id: str, local_path: str) -> None:
        self._remove_worker(model_id)
        card = self._cards_by_model_id.get(model_id)
        if card is not None:
            card.mark_downloaded()
        self.refresh_results()

    def _on_download_error(self, model_id: str, message: str) -> None:
        self._remove_worker(model_id)
        card = self._cards_by_model_id.get(model_id)
        if card is not None:
            card.set_busy(False)
        show_error(self, "Download failed", f"{model_id}\n\n{message}")

    def _on_download_cancelled(self, model_id: str) -> None:
        self._remove_worker(model_id)
        card = self._cards_by_model_id.get(model_id)
        if card is not None:
            card.set_busy(False)

    def _remove_worker(self, model_id: str) -> None:
        worker = self._workers.pop(model_id, None)
        if worker is not None:
            worker.deleteLater()
