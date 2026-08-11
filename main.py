from __future__ import annotations

import logging
import sys
import warnings

from config.constants import APP_DISPLAY_NAME, APP_NAME, APP_TAGLINE, ORGANIZATION_NAME
from config.settings import AppSettings


class _NoiseFilter(logging.Filter):
    _SUPPRESSED_SNIPPETS = (
        "The pynvml package is deprecated",
        "Megatron num_microbatches_calculator not found, using Apex version.",
        "OneLogger: Setting error_handling_strategy",
        "No exporters were provided. This means that no telemetry data will be collected.",
    )

    def filter(self, record: logging.LogRecord) -> bool:
        message = record.getMessage()
        return not any(snippet in message for snippet in self._SUPPRESSED_SNIPPETS)


def _configure_runtime_noise() -> None:
    warnings.filterwarnings(
        "ignore",
        message=r"The pynvml package is deprecated.*",
        category=FutureWarning,
    )

    root_logger = logging.getLogger()
    root_logger.addFilter(_NoiseFilter())


def main() -> int:
    try:
        from PyQt6.QtWidgets import QApplication
    except ImportError:
        print("PyQt6 is not installed. Install dependencies from requirements.txt first.")
        return 1

    _configure_runtime_noise()

    from ui.main_window import MainWindow
    from ui.theme import get_theme_stylesheet

    from core.gpu_manager import GPUManager
    from core.hub_browser import HubBrowser
    from core.model_manager import ModelManager

    app = QApplication(sys.argv)
    app.setApplicationName(APP_NAME)
    app.setApplicationDisplayName(APP_DISPLAY_NAME)
    app.setOrganizationName(ORGANIZATION_NAME)
    app.setStyle("Fusion")
    app.setStyleSheet(get_theme_stylesheet())

    settings = AppSettings()
    hub_browser = HubBrowser(settings.catalog_path)
    gpu_manager = GPUManager()
    model_manager = ModelManager(settings=settings, hub_browser=hub_browser)

    window = MainWindow(
        settings=settings,
        gpu_manager=gpu_manager,
        model_manager=model_manager,
        hub_browser=hub_browser,
    )
    window.show()
    window.statusBar().showMessage(f"{APP_TAGLINE} loaded.")
    return app.exec()


if __name__ == "__main__":
    raise SystemExit(main())
