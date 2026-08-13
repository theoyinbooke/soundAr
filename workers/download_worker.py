from __future__ import annotations

from PyQt6.QtCore import QThread, pyqtSignal

from core.model_manager import ModelManager


class DownloadCancelled(Exception):
    """Raised from progress callback to abort a download."""


class DownloadWorker(QThread):
    progress = pyqtSignal(float, float)
    finished = pyqtSignal(str)
    error = pyqtSignal(str)
    cancelled = pyqtSignal()

    def __init__(self, model_id: str, model_manager: ModelManager) -> None:
        super().__init__()
        self.model_id = model_id
        self.model_manager = model_manager
        self._cancelled = False

    def cancel(self) -> None:
        """Request cancellation. The download will abort at the next progress tick."""
        self._cancelled = True
        self.requestInterruption()

    def run(self) -> None:
        try:
            plan = self.model_manager.get_install_plan(self.model_id)
            model = self.model_manager.download_model(
                self.model_id,
                revision=str(plan["revision"]),
                progress_callback=self._on_progress,
            )
        except DownloadCancelled:
            self.model_manager.cleanup_partial_download(self.model_id)
            self.cancelled.emit()
            return
        except Exception as exc:
            if self._cancelled:
                self.model_manager.cleanup_partial_download(self.model_id)
                self.cancelled.emit()
                return
            self.error.emit(str(exc))
            return

        if self._cancelled:
            self.model_manager.cleanup_partial_download(self.model_id)
            self.cancelled.emit()
            return

        self.finished.emit(str(model["local_path"]))

    def _on_progress(self, downloaded: float, total: float) -> None:
        if self._cancelled:
            raise DownloadCancelled("Download cancelled by user")
        self.progress.emit(downloaded, total)
