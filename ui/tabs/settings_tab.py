"""Settings tab — model management and system info."""
from __future__ import annotations

import importlib.util
import sys
from typing import Any

from PyQt6.QtCore import QTimer, Qt
from PyQt6.QtWidgets import (
    QAbstractItemView,
    QFrame,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QPushButton,
    QScrollArea,
    QTableWidget,
    QTableWidgetItem,
    QVBoxLayout,
    QWidget,
)

from config.settings import AppSettings
from core.gpu_manager import GPUManager
from core.model_manager import ModelManager
from ui.dialogs.message_box import ask_confirmation, show_error
from ui.theme import COLORS


class SettingsTab(QWidget):
    def __init__(
        self,
        settings: AppSettings,
        model_manager: ModelManager,
        gpu_manager: GPUManager,
        parent: QWidget | None = None,
    ) -> None:
        super().__init__(parent)
        self.settings = settings
        self.model_manager = model_manager
        self.gpu_manager = gpu_manager

        self.setStyleSheet("background: transparent;")
        self._build_ui()
        self.refresh()

    def _build_ui(self) -> None:
        scroll = QScrollArea(self)
        scroll.setWidgetResizable(True)
        scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAlwaysOff)

        content = QWidget()
        content.setStyleSheet("background: transparent;")
        layout = QVBoxLayout(content)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(20)

        # General settings card
        general_card = self._build_card("General")
        general_layout = general_card.layout()

        self._cache_label = QLabel("")
        self._cache_label.setObjectName("metadata")
        general_layout.addWidget(QLabel("Cache directory:"))
        general_layout.addWidget(self._cache_label)

        self._filter_label = QLabel("")
        self._filter_label.setObjectName("metadata")
        general_layout.addWidget(QLabel("Default task filter:"))
        general_layout.addWidget(self._filter_label)

        self._limit_label = QLabel("")
        self._limit_label.setObjectName("metadata")
        general_layout.addWidget(QLabel("Results limit:"))
        general_layout.addWidget(self._limit_label)

        layout.addWidget(general_card)

        # Downloaded models card
        models_card = self._build_card("Downloaded models")
        models_layout = models_card.layout()

        models_intro = QLabel(
            "Manage local model installs and remove models you no longer need."
        )
        models_intro.setObjectName("metadata")
        models_layout.addWidget(models_intro)

        models_meta_row = QHBoxLayout()
        models_meta_row.setContentsMargins(0, 0, 0, 0)
        models_meta_row.setSpacing(10)

        self._models_count = QLabel("")
        self._models_count.setStyleSheet(
            f"""
            QLabel {{
                background: {COLORS['bg_raised']};
                color: {COLORS['text_secondary']};
                border: 1px solid {COLORS['border_default']};
                border-radius: 10px;
                padding: 4px 10px;
                font-size: 11px;
                font-weight: 600;
            }}
            """
        )
        models_meta_row.addWidget(self._models_count, 0, Qt.AlignmentFlag.AlignLeft)

        models_hint = QLabel("Deleting a model removes its local files and registry entry.")
        models_hint.setObjectName("metadata")
        models_meta_row.addWidget(models_hint)
        models_meta_row.addStretch()
        models_layout.addLayout(models_meta_row)

        self._models_table = QTableWidget()
        self._models_table.setObjectName("settingsModelsTable")
        self._models_table.setColumnCount(5)
        self._models_table.setHorizontalHeaderLabels([
            "Model", "Task", "Runtime", "Added", ""
        ])
        header = self._models_table.horizontalHeader()
        if header is not None:
            header.setSectionResizeMode(0, QHeaderView.ResizeMode.Fixed)
            header.setSectionResizeMode(1, QHeaderView.ResizeMode.Fixed)
            header.setSectionResizeMode(2, QHeaderView.ResizeMode.Fixed)
            header.setSectionResizeMode(3, QHeaderView.ResizeMode.Fixed)
            header.setSectionResizeMode(4, QHeaderView.ResizeMode.Fixed)
            header.setDefaultAlignment(Qt.AlignmentFlag.AlignLeft | Qt.AlignmentFlag.AlignVCenter)
        self._models_table.setSelectionBehavior(
            QTableWidget.SelectionBehavior.SelectRows
        )
        self._models_table.setSelectionMode(QAbstractItemView.SelectionMode.SingleSelection)
        self._models_table.setEditTriggers(QAbstractItemView.EditTrigger.NoEditTriggers)
        self._models_table.setFocusPolicy(Qt.FocusPolicy.NoFocus)
        self._models_table.setShowGrid(False)
        self._models_table.setWordWrap(False)
        self._models_table.setAlternatingRowColors(False)
        self._models_table.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        self._models_table.setVerticalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAsNeeded)
        self._models_table.setVerticalScrollMode(QAbstractItemView.ScrollMode.ScrollPerPixel)
        self._models_table.setMinimumHeight(190)
        self._models_table.verticalHeader().setVisible(False)
        self._models_table.verticalHeader().setDefaultSectionSize(56)
        self._models_table.setStyleSheet(
            f"""
            QTableWidget#settingsModelsTable {{
                background: #fcfbf8;
                border: 1px solid {COLORS['border_default']};
                border-radius: 14px;
                color: {COLORS['text_primary']};
                gridline-color: transparent;
                font-size: 13px;
            }}
            QTableWidget#settingsModelsTable::item {{
                padding: 0 14px;
                border-bottom: 1px solid #ece9e2;
            }}
            QTableWidget#settingsModelsTable::item:selected {{
                background: #f4f8ff;
                color: {COLORS['text_primary']};
            }}
            QHeaderView::section {{
                background: transparent;
                color: {COLORS['text_tertiary']};
                padding: 10px 14px;
                border: none;
                border-bottom: 1px solid #ece9e2;
                font-size: 11px;
                font-weight: 600;
            }}
            """
        )

        models_layout.addWidget(self._models_table)

        self._models_empty = QLabel(
            "No models downloaded yet. Install models from the Model hub to manage them here."
        )
        self._models_empty.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self._models_empty.setWordWrap(True)
        self._models_empty.setStyleSheet(
            f"""
            QLabel {{
                color: {COLORS['text_secondary']};
                background: #fcfbf8;
                border: 1px dashed {COLORS['border_default']};
                border-radius: 14px;
                padding: 24px;
                font-size: 13px;
            }}
            """
        )
        self._models_empty.hide()
        models_layout.addWidget(self._models_empty)
        layout.addWidget(models_card)

        # System info card
        sys_card = self._build_card("System info")
        sys_layout = sys_card.layout()

        self._sys_info = QLabel("")
        self._sys_info.setObjectName("metadata")
        self._sys_info.setWordWrap(True)
        self._sys_info.setTextInteractionFlags(
            Qt.TextInteractionFlag.TextSelectableByMouse
        )
        sys_layout.addWidget(self._sys_info)

        layout.addWidget(sys_card)
        layout.addStretch()

        scroll.setWidget(content)

        root = QVBoxLayout(self)
        root.setContentsMargins(0, 0, 0, 0)
        root.addWidget(scroll)

    def _build_card(self, title: str) -> QFrame:
        card = QFrame(self)
        card.setObjectName("card")
        layout = QVBoxLayout(card)
        layout.setContentsMargins(20, 16, 20, 16)
        layout.setSpacing(8)

        label = QLabel(title)
        label.setObjectName("sectionTitle")
        layout.addWidget(label)

        return card

    def refresh(self) -> None:
        """Refresh all settings displays."""
        # General
        self._cache_label.setText(self.settings.model_cache_dir)
        self._filter_label.setText(self.settings.default_task_filter)
        self._limit_label.setText(str(self.settings.hub_results_limit))

        # Models table
        self._refresh_models_table()

        # System info
        self._refresh_system_info()

    def _refresh_models_table(self) -> None:
        models = self.model_manager.list_downloaded_models()
        self._models_count.setText(f"{len(models)} installed")

        self._models_table.clearContents()

        if not models:
            self._models_table.hide()
            self._models_empty.show()
            self._models_table.setRowCount(0)
            return

        self._models_empty.hide()
        self._models_table.show()
        self._models_table.setRowCount(len(models))
        self._update_models_table_layout()

        for row, model in enumerate(models):
            model_id = model.get("model_id", "")
            self._models_table.setRowHeight(row, 56)
            self._models_table.setCellWidget(
                row,
                0,
                self._build_model_cell(
                    model_id,
                    model.get("task", ""),
                    model.get("engine", ""),
                ),
            )
            self._models_table.setCellWidget(
                row,
                1,
                self._build_badge_cell(model.get("task", ""), "task"),
            )
            self._models_table.setCellWidget(
                row,
                2,
                self._build_badge_cell(model.get("engine", ""), "engine"),
            )
            downloaded = model.get("downloaded_at", "")
            if downloaded:
                downloaded = downloaded[:10]  # Just the date
            downloaded_item = QTableWidgetItem(downloaded or "Unknown")
            downloaded_item.setTextAlignment(
                int(Qt.AlignmentFlag.AlignCenter | Qt.AlignmentFlag.AlignVCenter)
            )
            self._models_table.setItem(row, 3, downloaded_item)
            self._models_table.setCellWidget(row, 4, self._build_delete_cell(model_id))

        QTimer.singleShot(0, self._update_models_table_layout)

    def _update_models_table_layout(self) -> None:
        visible_rows = 5
        row_height = self._models_table.verticalHeader().defaultSectionSize()
        header_height = self._models_table.horizontalHeader().height() or 44
        frame_height = (self._models_table.frameWidth() * 2) + 6
        self._models_table.setFixedHeight(
            header_height + (row_height * visible_rows) + frame_height
        )

        viewport_width = self._models_table.viewport().width()
        if viewport_width <= 0:
            viewport_width = max(self._models_table.width() - 24, 900)

        task_width = 92
        runtime_width = 164
        added_width = 124
        action_width = 144
        spacing = 8

        model_width = max(
            360,
            viewport_width - (task_width + runtime_width + added_width + action_width + spacing),
        )

        self._models_table.setColumnWidth(0, model_width)
        self._models_table.setColumnWidth(1, task_width)
        self._models_table.setColumnWidth(2, runtime_width)
        self._models_table.setColumnWidth(3, added_width)
        self._models_table.setColumnWidth(4, action_width)

    def resizeEvent(self, event: Any) -> None:
        super().resizeEvent(event)
        if hasattr(self, "_models_table"):
            QTimer.singleShot(0, self._update_models_table_layout)

    def _build_model_cell(self, model_id: str, task: str, engine: str) -> QWidget:
        container = QWidget(self._models_table)
        layout = QVBoxLayout(container)
        layout.setContentsMargins(14, 7, 12, 7)
        layout.setSpacing(2)

        title = QLabel(model_id)
        title.setStyleSheet(
            f"color: {COLORS['text_primary']}; font-size: 14px; font-weight: 600;"
        )
        title.setTextInteractionFlags(Qt.TextInteractionFlag.TextSelectableByMouse)
        layout.addWidget(title)

        caption_parts = []
        if task:
            caption_parts.append(task.upper())
        if engine:
            caption_parts.append(engine)
        caption_text = "Local model ready for use"
        if caption_parts:
            caption_text = " • ".join(caption_parts)

        caption = QLabel(caption_text)
        caption.setStyleSheet(
            f"color: {COLORS['text_secondary']}; font-size: 11px;"
        )
        layout.addWidget(caption)
        return container

    def _build_badge_cell(self, text: str, kind: str) -> QWidget:
        container = QWidget(self._models_table)
        layout = QHBoxLayout(container)
        layout.setContentsMargins(10, 0, 10, 0)
        layout.setSpacing(0)
        layout.setAlignment(Qt.AlignmentFlag.AlignCenter)

        label = QLabel((text or "n/a").upper() if kind == "task" else (text or "n/a"))
        if kind == "task":
            background = "#eef4ff" if text.lower() == "stt" else "#fff4e8"
            foreground = "#2563eb" if text.lower() == "stt" else "#c56a00"
        else:
            background = "#f3f6f8"
            foreground = COLORS["text_secondary"]

        label.setStyleSheet(
            f"""
            QLabel {{
                background: {background};
                color: {foreground};
                border-radius: 10px;
                padding: 4px 10px;
                font-size: 11px;
                font-weight: 600;
            }}
            """
        )
        layout.addWidget(label)
        return container

    def _build_delete_cell(self, model_id: str) -> QWidget:
        container = QWidget(self._models_table)
        layout = QHBoxLayout(container)
        layout.setContentsMargins(8, 0, 8, 0)
        layout.setSpacing(0)
        layout.setAlignment(Qt.AlignmentFlag.AlignCenter)

        delete_btn = QPushButton("Delete")
        delete_btn.setCursor(Qt.CursorShape.PointingHandCursor)
        delete_btn.setFixedSize(84, 30)
        delete_btn.setStyleSheet(
            """
            QPushButton {
                background: #fff5f5;
                color: #dc2626;
                border: 1px solid #fecaca;
                border-radius: 8px;
                font-size: 12px;
                font-weight: 600;
            }
            QPushButton:hover {
                background: #fee2e2;
                border-color: #fca5a5;
            }
            QPushButton:pressed {
                background: #fecaca;
            }
            """
        )
        delete_btn.clicked.connect(
            lambda checked, mid=model_id: self._on_delete_model(mid)
        )
        layout.addWidget(delete_btn)
        return container

    def _refresh_system_info(self) -> None:
        gpu_info = self.gpu_manager.get_gpu_info()
        vram = self.gpu_manager.get_vram_usage()

        lines = [
            f"Python: {sys.version.split()[0]}",
            f"GPU: {gpu_info.get('name', 'N/A')}",
            f"CUDA: {gpu_info.get('cuda_version', 'N/A')}",
        ]

        if gpu_info.get("cuda_available"):
            lines.append(
                f"VRAM: {vram.get('used_mb', 0):.0f} / {vram.get('total_mb', 0):.0f} MB"
            )

        # Check optional dependencies
        deps = []
        try:
            import PyQt6
            deps.append(f"PyQt6: {PyQt6.QtCore.PYQT_VERSION_STR}")
        except Exception:
            deps.append("PyQt6: N/A")

        dependency_specs = {
            "nemo": "nemo.collections.asr",
            "TTS": "TTS",
            "kokoro": "kokoro",
        }
        for name, module_name in dependency_specs.items():
            installed = importlib.util.find_spec(module_name) is not None
            deps.append(f"{name}: {'installed' if installed else 'not installed'}")

        lines.append("")
        lines.append("Dependencies:")
        lines.extend(f"  {d}" for d in deps)

        self._sys_info.setText("\n".join(lines))

    def _on_delete_model(self, model_id: str) -> None:
        if ask_confirmation(
            self,
            "Delete model",
            f"Delete '{model_id}' and its local files?",
        ):
            try:
                self.model_manager.delete_model(model_id)
                self._refresh_models_table()
            except Exception as e:
                show_error(self, "Delete failed", str(e))
