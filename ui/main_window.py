"""Main application window — sidebar + stacked content layout per design system v2."""
from __future__ import annotations

from PyQt6.QtCore import Qt
from PyQt6.QtWidgets import (
    QHBoxLayout,
    QLabel,
    QMainWindow,
    QStackedWidget,
    QVBoxLayout,
    QWidget,
)

from config.constants import (
    APP_DISPLAY_NAME,
    APP_TAGLINE,
    WINDOW_MIN_HEIGHT,
    WINDOW_MIN_WIDTH,
)
from config.settings import AppSettings
from core.gpu_manager import GPUManager
from core.hub_browser import HubBrowser
from core.model_manager import ModelManager
from ui.tabs.compare_tab import CompareTab
from ui.tabs.hub_tab import HubTab
from ui.tabs.realtime_tab import RealtimeTab
from ui.tabs.settings_tab import SettingsTab
from ui.tabs.stt_tab import STTTab
from ui.tabs.tts_tab import TTSTab
from ui.theme import COLORS
from ui.widgets.gpu_status import GPUStatusPill
from ui.widgets.sidebar import Sidebar


# Map sidebar keys to stack indices
_NAV_KEYS = ["hub", "stt", "tts", "realtime", "compare", "settings"]


class MainWindow(QMainWindow):
    def __init__(
        self,
        settings: AppSettings,
        gpu_manager: GPUManager,
        model_manager: ModelManager,
        hub_browser: HubBrowser,
    ) -> None:
        super().__init__()
        self.settings = settings
        self.gpu_manager = gpu_manager
        self.model_manager = model_manager
        self.hub_browser = hub_browser

        self.setWindowTitle(f"{APP_DISPLAY_NAME} | {APP_TAGLINE}")
        self.resize(settings.window_width, settings.window_height)
        self.setMinimumSize(WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT)

        # --- Sidebar ---
        self.sidebar = Sidebar(self)
        self.sidebar.nav_changed.connect(self._on_nav_changed)

        # --- Content area ---
        content_widget = QWidget(self)
        content_widget.setStyleSheet(f"background-color: {COLORS['bg_primary']};")
        content_layout = QVBoxLayout(content_widget)
        content_layout.setContentsMargins(32, 28, 32, 16)
        content_layout.setSpacing(0)

        # Page header (title + GPU pill)
        self.page_header = self._build_page_header()
        content_layout.addWidget(self.page_header)
        content_layout.addSpacing(28)

        # Stacked pages
        self.page_stack = QStackedWidget(self)
        self.page_stack.setStyleSheet("background: transparent;")

        # Hub page (implemented)
        self.hub_tab = HubTab(
            hub_browser=self.hub_browser,
            model_manager=self.model_manager,
            settings=self.settings,
        )
        self.page_stack.addWidget(self.hub_tab)  # index 0

        # STT tab (implemented)
        self.stt_tab = STTTab(
            model_manager=self.model_manager,
            gpu_manager=self.gpu_manager,
        )
        self.page_stack.addWidget(self.stt_tab)  # index 1

        # TTS tab (implemented)
        self.tts_tab = TTSTab(
            model_manager=self.model_manager,
            gpu_manager=self.gpu_manager,
        )
        self.page_stack.addWidget(self.tts_tab)  # index 2

        # Realtime tab (implemented)
        self.realtime_tab = RealtimeTab(
            model_manager=self.model_manager,
            gpu_manager=self.gpu_manager,
        )
        self.page_stack.addWidget(self.realtime_tab)  # index 3

        # Compare tab (implemented)
        self.compare_tab = CompareTab(
            model_manager=self.model_manager,
            gpu_manager=self.gpu_manager,
        )
        self.page_stack.addWidget(self.compare_tab)  # index 4

        # Settings tab (implemented)
        self.settings_tab = SettingsTab(
            settings=self.settings,
            model_manager=self.model_manager,
            gpu_manager=self.gpu_manager,
        )
        self.page_stack.addWidget(self.settings_tab)  # index 5

        initial_index = min(
            max(0, int(self.settings.last_active_tab)),
            self.page_stack.count() - 1,
        )
        initial_key = _NAV_KEYS[initial_index]
        self.sidebar.set_active(initial_key)
        self.page_stack.setCurrentIndex(initial_index)
        content_layout.addWidget(self.page_stack, 1)

        # --- Root layout: sidebar | content ---
        root = QWidget(self)
        root_layout = QHBoxLayout(root)
        root_layout.setContentsMargins(0, 0, 0, 0)
        root_layout.setSpacing(0)
        root_layout.addWidget(self.sidebar)
        root_layout.addWidget(content_widget, 1)

        self.setCentralWidget(root)
        self.statusBar().showMessage("Curated catalog ready.")
        self._on_nav_changed(initial_key)

    # ── Header ──────────────────────────────────────────────

    def _build_page_header(self) -> QWidget:
        header = QWidget(self)
        header.setStyleSheet("background: transparent;")
        layout = QHBoxLayout(header)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(0)

        # Left: title + subtitle
        left = QWidget(header)
        left.setStyleSheet("background: transparent;")
        left_layout = QVBoxLayout(left)
        left_layout.setContentsMargins(0, 0, 0, 0)
        left_layout.setSpacing(6)

        self.page_title = QLabel("Model hub")
        self.page_title.setObjectName("title")

        self.page_subtitle = QLabel("Browse and download curated speech models")
        self.page_subtitle.setObjectName("subtitle")

        left_layout.addWidget(self.page_title)
        left_layout.addWidget(self.page_subtitle)

        # Right: GPU pill
        gpu_info = self.gpu_manager.get_gpu_info()
        self.gpu_pill = GPUStatusPill(gpu_info, header)

        layout.addWidget(left, 1)
        layout.addWidget(self.gpu_pill, 0, Qt.AlignmentFlag.AlignTop)

        return header

    # ── Navigation ──────────────────────────────────────────

    _PAGE_TITLES = {
        "hub": ("Model hub", "Browse and download curated speech models"),
        "stt": ("Speech to text", "Transcribe audio with local models"),
        "tts": ("Text to speech", "Synthesize speech from text"),
        "realtime": ("Live transcription", "Real-time speech recognition"),
        "compare": ("Compare models", "Side-by-side model evaluation"),
        "settings": ("Settings", "Configure app preferences"),
    }

    def _on_nav_changed(self, key: str) -> None:
        idx = _NAV_KEYS.index(key) if key in _NAV_KEYS else 0
        self.page_stack.setCurrentIndex(idx)

        title, subtitle = self._PAGE_TITLES.get(key, ("Model hub", ""))
        self.page_title.setText(title)
        self.page_subtitle.setText(subtitle)

        # Refresh model lists when navigating to relevant tabs
        if key == "stt":
            self.stt_tab.refresh_model_list()
        elif key == "tts":
            self.tts_tab.refresh_model_list()
        elif key == "realtime":
            self.realtime_tab.refresh_model_list()
        elif key == "compare":
            self.compare_tab.refresh_model_list()
        elif key == "settings":
            self.settings_tab.refresh()

    # ── Lifecycle ───────────────────────────────────────────

    def closeEvent(self, event) -> None:  # type: ignore[override]
        self.settings.last_active_tab = self.page_stack.currentIndex()
        self.settings.window_width = self.width()
        self.settings.window_height = self.height()
        self.settings.save()
        super().closeEvent(event)
