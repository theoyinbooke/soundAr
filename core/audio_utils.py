"""Audio loading, inspection, resampling, export, and waveform envelope.

Stateless module-level functions — no Qt dependency.
"""
from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

import numpy as np
import soundfile as sf

try:
    import librosa
except ImportError:  # pragma: no cover
    librosa = None  # type: ignore[assignment]

try:
    from pydub import AudioSegment
except ImportError:  # pragma: no cover
    AudioSegment = None  # type: ignore[assignment]


# ── AudioInfo dataclass ────────────────────────────────────

@dataclass(frozen=True)
class AudioInfo:
    """Metadata about an audio file."""
    path: str
    format: str
    sample_rate: int
    channels: int
    frames: int
    duration_seconds: float
    subtype: str


# ── Inspection ─────────────────────────────────────────────

def inspect_audio(path: str | Path) -> AudioInfo:
    """Return metadata for an audio file.

    Uses soundfile.info() for wav/flac/ogg (fast, no full load).
    Falls back to pydub for mp3/m4a/webm.
    """
    path = Path(path)
    suffix = path.suffix.lower()

    # Try soundfile first (handles wav, flac, ogg natively)
    try:
        info = sf.info(str(path))
        return AudioInfo(
            path=str(path),
            format=suffix.lstrip("."),
            sample_rate=info.samplerate,
            channels=info.channels,
            frames=info.frames,
            duration_seconds=info.duration,
            subtype=info.subtype,
        )
    except Exception:
        pass

    # Fallback: pydub for mp3/m4a/webm and other formats
    if AudioSegment is None:
        raise RuntimeError(
            f"Cannot inspect {path.name}: soundfile failed and pydub is not installed"
        )

    seg = AudioSegment.from_file(str(path))
    frames = int(len(seg.get_array_of_samples()) / seg.channels)
    return AudioInfo(
        path=str(path),
        format=suffix.lstrip("."),
        sample_rate=seg.frame_rate,
        channels=seg.channels,
        frames=frames,
        duration_seconds=seg.duration_seconds,
        subtype="int16",
    )


# ── Loading ────────────────────────────────────────────────

def load_audio(
    path: str | Path,
    target_sr: int = 16_000,
    mono: bool = True,
) -> tuple[np.ndarray, int]:
    """Load audio file, resample to *target_sr*, optionally mix to mono.

    Returns (audio_array, sample_rate).
    """
    if librosa is None:
        raise RuntimeError("librosa is required for load_audio()")

    audio, sr = librosa.load(str(path), sr=target_sr, mono=mono)
    return audio, sr


def load_audio_raw(path: str | Path) -> tuple[np.ndarray, int]:
    """Load audio at its native sample rate (for waveform display).

    Returns (audio_array, sample_rate).
    """
    if librosa is None:
        raise RuntimeError("librosa is required for load_audio_raw()")

    audio, sr = librosa.load(str(path), sr=None, mono=True)
    return audio, sr


# ── Resampling ─────────────────────────────────────────────

def resample(audio: np.ndarray, orig_sr: int, target_sr: int) -> np.ndarray:
    """Resample audio from *orig_sr* to *target_sr*."""
    if orig_sr == target_sr:
        return audio
    if librosa is None:
        raise RuntimeError("librosa is required for resample()")
    return librosa.resample(audio, orig_sr=orig_sr, target_sr=target_sr)


# ── Export / Save ──────────────────────────────────────────

def numpy_to_pydub(audio: np.ndarray, sample_rate: int) -> AudioSegment:
    """Convert a float32 numpy array to a pydub AudioSegment."""
    if AudioSegment is None:
        raise RuntimeError("pydub is required for numpy_to_pydub()")

    # Clip and convert float32 → int16
    clipped = np.clip(audio, -1.0, 1.0)
    int16_data = (clipped * 32767).astype(np.int16)
    return AudioSegment(
        data=int16_data.tobytes(),
        sample_width=2,
        frame_rate=sample_rate,
        channels=1,
    )


def save_audio(
    path: str | Path,
    audio: np.ndarray,
    sample_rate: int,
    format: str | None = None,
) -> None:
    """Save audio array to file.

    Uses soundfile for wav/flac/ogg; pydub for mp3/m4a/webm.
    """
    path = Path(path)
    fmt = (format or path.suffix.lstrip(".")).lower()

    if fmt in ("wav", "flac", "ogg"):
        sf.write(str(path), audio, sample_rate, format=fmt.upper())
    else:
        # mp3, m4a, webm — use pydub
        seg = numpy_to_pydub(audio, sample_rate)
        seg.export(str(path), format=fmt)


# ── Waveform envelope ─────────────────────────────────────

def compute_waveform_envelope(audio: np.ndarray, num_bins: int) -> np.ndarray:
    """Compute a downsampled peak-amplitude envelope.

    Splits *audio* into *num_bins* chunks and takes max(abs()) per chunk.
    Result is normalized to [0, 1].
    """
    if num_bins <= 0:
        return np.array([], dtype=np.float32)

    length = len(audio)
    if length == 0:
        return np.zeros(num_bins, dtype=np.float32)

    # Compute chunk boundaries
    indices = np.linspace(0, length, num_bins + 1, dtype=int)
    envelope = np.empty(num_bins, dtype=np.float32)

    for i in range(num_bins):
        chunk = audio[indices[i]:indices[i + 1]]
        envelope[i] = np.max(np.abs(chunk)) if len(chunk) > 0 else 0.0

    # Normalize to [0, 1]
    peak = envelope.max()
    if peak > 0:
        envelope /= peak

    return envelope
