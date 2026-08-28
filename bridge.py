#!/usr/bin/env python3
"""Small JSON bridge between the Tauri shell and soundAr's Python engines."""
from __future__ import annotations

import argparse
import contextlib
import io
import json
import math
import os
import re
import sys
import time
import uuid
import xml.etree.ElementTree as ET
from datetime import datetime, timezone
from pathlib import Path

import numpy as np

os.environ.setdefault("PYTORCH_CUDA_ALLOC_CONF", "expandable_segments:True")
os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")

from config.settings import AppSettings
from core.audio_utils import (
    compute_waveform_envelope,
    inspect_audio,
    load_audio,
    load_audio_raw,
    save_audio,
)
from core.benchmark import BenchmarkCollector
from core.gpu_manager import GPUManager
from core.engine_contract import EngineContractRegistry
from core.hub_browser import HubBrowser
from core.model_manager import ModelManager
from core.music_engine import MusicEngine
from core.tts_engine import TTSEngine
from core.stt_engine import STTEngine
from core.speaker_verifier import SpeakerVerifier
from core.speaker_diarizer import (
    average_link_cluster,
    build_speech_windows,
    extract_window_audio,
    turns_from_windows,
)
from core.forced_aligner import ForcedAligner


class Runtime:
    """Persistent inference runtime with a single warm model cache."""

    def __init__(self) -> None:
        self.engine_scope = os.environ.get("SOUNDAR_ENGINE_SCOPE", "foundation")
        self.engine_runtime = os.environ.get("SOUNDAR_ENGINE_RUNTIME", "development")
        self.settings = AppSettings()
        self.hub = HubBrowser(self.settings.catalog_path)
        self.manager = ModelManager(self.settings, self.hub)
        self.gpu = GPUManager()
        self.engine = TTSEngine(self.gpu)
        self.music_engine = MusicEngine(self.gpu)
        self.stt_engine = STTEngine(self.gpu)
        self.speaker_verifier = SpeakerVerifier(self.gpu)
        self.forced_aligner = ForcedAligner(self.gpu)
        self.contracts = EngineContractRegistry(Path(self.settings.catalog_path).parent / "engine_manifests.json")
        self.event_sink = None

    def dispatch(self, request: dict[str, object]) -> dict[str, object]:
        operation = str(request.get("operation", "synthesize"))
        if operation == "synthesize":
            return self.synthesize(request)
        if operation == "generate_music":
            return self.generate_music(request)
        if operation == "load":
            return self.load(request)
        if operation == "unload":
            return self.unload()
        if operation == "analyze_audio":
            return self.analyze_audio(request)
        if operation == "prepare_voice_reference":
            return self.prepare_voice_reference(request)
        if operation == "prepare_transcription_audio":
            return self.prepare_transcription_audio(request)
        if operation == "master_audio":
            return self.master_audio(request)
        if operation == "transcribe":
            return self.transcribe(request)
        if operation == "compare_speakers":
            return self.compare_speakers(request)
        if operation == "diarize":
            return self.diarize(request)
        if operation == "align_transcript":
            return self.align_transcript(request)
        if operation == "health":
            return {
                "protocol_version": 1,
                "status": "ready",
                "device": self.gpu.get_device(),
                "engine_scope": self.engine_scope,
                "engine_runtime": self.engine_runtime,
                "process_id": os.getpid(),
                "loaded_models": self.loaded_models(),
            }
        if operation == "capabilities":
            return {"protocol_version": 1, "engines": self.contracts.list()}
        raise ValueError(f"Unsupported runtime operation: {operation}")

    def loaded_models(self) -> list[str]:
        return [
            model_id
            for model_id in (
                self.engine.loaded_model_id,
                self.music_engine.loaded_model_id,
                self.stt_engine.loaded_model_id,
                self.speaker_verifier.loaded_model_id,
                self.forced_aligner.loaded_model_id,
            )
            if model_id
        ]

    def unload(self) -> dict[str, object]:
        unloaded = self.loaded_models()
        self.engine.unload_model()
        self.music_engine.unload_model()
        self.stt_engine.unload_model()
        self.speaker_verifier.unload_model()
        self.forced_aligner.unload_model()
        return {
            "status": "unloaded",
            "engine_scope": self.engine_scope,
            "unloaded_models": unloaded,
            "vram": self.gpu.get_vram_usage(),
        }

    def load(self, request: dict[str, object]) -> dict[str, object]:
        model_id = str(request.get("model_id", "")).strip()
        if not model_id:
            raise ValueError("Model loading requires an installed model.")
        installed = self.manager.get_downloaded_model(model_id)
        if installed is None:
            raise ValueError(f"Model is not installed: {model_id}")
        engine_name = str(installed.get("engine", self.manager.detect_engine(model_id)))
        self._validate_scope(engine_name)
        model_path = str(installed.get("local_path", ""))
        task = str(installed.get("task", ""))
        self.unload()
        if task == "tts":
            self.engine.load_model(model_id, model_path, engine_name)
        elif task == "music":
            self.music_engine.load_model(model_id, model_path, engine_name)
        elif task == "stt":
            self.stt_engine.load_model(model_id, model_path, engine_name)
        elif task == "speaker-verification":
            self.speaker_verifier.load_model(model_id, model_path)
        elif task == "alignment":
            self.forced_aligner.load_model(model_id, model_path)
        else:
            raise ValueError(f"Unsupported model task for loading: {task or 'unknown'}")
        return {
            "status": "loaded",
            "model_id": model_id,
            "engine": engine_name,
            "task": task,
            "device": self.gpu.get_device(),
            "vram": self.gpu.get_vram_usage(),
        }

    def analyze_audio(self, request: dict[str, object]) -> dict[str, object]:
        raw_path = str(request.get("audio_path", "")).strip()
        if not raw_path:
            raise ValueError("Audio analysis requires a file path.")
        path = Path(raw_path).expanduser()
        if not path.is_file():
            raise ValueError(f"Reference audio was not found: {path}")

        return analyze_audio_path(path)

    def prepare_voice_reference(self, request: dict[str, object]) -> dict[str, object]:
        raw_path = str(request.get("audio_path", "")).strip()
        raw_output = str(request.get("output_path", "")).strip()
        if not raw_path or not raw_output:
            raise ValueError("Voice preparation requires input and output paths.")
        source = Path(raw_path).expanduser().resolve()
        output = Path(raw_output).expanduser().resolve()
        if not source.is_file():
            raise ValueError(f"Reference audio was not found: {source}")
        if output.parent != source.parent or output.suffix.lower() != ".wav":
            raise ValueError("Processed voice audio must be a WAV beside its managed original.")

        source_info = inspect_audio(source)
        audio, sample_rate = load_audio(source, target_sr=24_000, mono=True)
        if audio.size == 0:
            raise ValueError("Reference audio contains no samples.")
        audio = audio.astype("float32", copy=False)
        audio -= float(audio.mean())
        original_samples = int(audio.size)

        selection_start = float(request.get("trim_start_seconds", 0) or 0)
        selection_end = float(request.get("trim_end_seconds", original_samples / sample_rate) or (original_samples / sample_rate))
        source_duration = original_samples / sample_rate
        if selection_start < 0 or selection_end <= selection_start or selection_end > source_duration + 0.001:
            raise ValueError("Reference trim must be a non-empty range inside the original audio.")
        selection_start_sample = min(original_samples, round(selection_start * sample_rate))
        selection_end_sample = min(original_samples, round(selection_end * sample_rate))
        audio = audio[selection_start_sample:selection_end_sample]
        if audio.size == 0:
            raise ValueError("Reference trim contains no samples.")

        trim_start = 0
        trim_end = int(audio.size)
        remove_silence = bool(request.get("remove_silence", True))
        if remove_silence:
            try:
                import librosa

                audio, indices = librosa.effects.trim(audio, top_db=35, frame_length=2048, hop_length=256)
                trim_start, trim_end = int(indices[0]), int(indices[1])
            except Exception:
                # Preparation still remains useful when optional trimming support is unavailable.
                pass
        if audio.size == 0:
            raise ValueError("Reference contains no speech-like audio after trimming.")

        pre_normalization_peak = float(abs(audio).max())
        normalize = bool(request.get("normalize", True))
        peak_target_dbfs = float(request.get("peak_target_dbfs", -1.0) or -1.0)
        if peak_target_dbfs > 0 or peak_target_dbfs < -12:
            raise ValueError("Reference peak target must be between -12 and 0 dBFS.")
        target_peak = 10 ** (peak_target_dbfs / 20.0)
        gain = target_peak / pre_normalization_peak if normalize and pre_normalization_peak > 1e-6 else 1.0
        audio = (audio * min(gain, 12.0)).clip(-1.0, 1.0).astype("float32")
        temporary = output.with_suffix(".wav.partial")
        save_audio(temporary, audio, sample_rate, "wav")
        os.replace(temporary, output)

        analysis = analyze_audio_path(output)
        processing = {
            "schema_version": 2,
            "source_sample_rate": source_info.sample_rate,
            "output_sample_rate": sample_rate,
            "mono": True,
            "dc_offset_removed": True,
            "remove_silence": remove_silence,
            "normalize": normalize,
            "edge_trim_db": 35 if remove_silence else None,
            "selection_start_seconds": round(selection_start, 4),
            "selection_end_seconds": round(selection_end, 4),
            "trim_start_seconds": round(trim_start / sample_rate, 4),
            "trim_end_seconds": round(((selection_end_sample - selection_start_sample) - trim_end) / sample_rate, 4),
            "peak_target_dbfs": peak_target_dbfs if normalize else None,
            "gain_db": round(20.0 * __import__("math").log10(max(gain, 1e-6)), 2),
        }
        return {"audio_path": str(output), "analysis": analysis, "processing": processing}

    def prepare_transcription_audio(self, request: dict[str, object]) -> dict[str, object]:
        import numpy as np

        raw_path = str(request.get("audio_path", "")).strip()
        raw_output = str(request.get("output_path", "")).strip()
        if not raw_path or not raw_output:
            raise ValueError("Transcription cleanup requires input and output paths.")
        source = Path(raw_path).expanduser().resolve()
        output = Path(raw_output).expanduser().resolve()
        if not source.is_file():
            raise ValueError(f"Transcription audio was not found: {source}")
        if output.parent != source.parent or output.suffix.lower() != ".wav":
            raise ValueError("Cleaned transcription audio must be a WAV beside its managed source.")

        audio, sample_rate = load_audio(source, target_sr=16_000, mono=True)
        audio = np.asarray(audio, dtype=np.float32)
        if audio.size < sample_rate // 5:
            raise ValueError("Transcription audio must contain at least 0.2 seconds of samples.")
        audio -= float(audio.mean())
        original_peak = float(np.max(np.abs(audio)))

        # A 70 Hz high-pass removes DC/rumble without suppressing speech fundamentals.
        cutoff_hz = 70.0
        try:
            from scipy.signal import butter, sosfilt

            high_passed = sosfilt(
                butter(2, cutoff_hz, btype="highpass", fs=sample_rate, output="sos"),
                audio,
            ).astype(np.float32)
        except Exception:
            coefficient = float(np.exp(-2.0 * np.pi * cutoff_hz / sample_rate))
            high_passed = np.empty_like(audio)
            previous_input = 0.0
            previous_output = 0.0
            for index, sample in enumerate(audio):
                value = coefficient * (previous_output + float(sample) - previous_input)
                high_passed[index] = value
                previous_input = float(sample)
                previous_output = value

        frame_length = max(160, round(sample_rate * 0.02))
        hop_length = max(80, frame_length // 2)
        frame_starts = np.arange(0, max(1, high_passed.size - frame_length + 1), hop_length)
        if not frame_starts.size:
            frame_starts = np.array([0])
        rms = np.array([
            float(np.sqrt(np.mean(np.square(high_passed[start:start + frame_length], dtype=np.float64)) + 1e-12))
            for start in frame_starts
        ], dtype=np.float32)
        noise_floor_before = float(np.percentile(rms, 20))
        threshold = max(noise_floor_before * 2.5, 10 ** (-52.0 / 20.0))
        target = np.clip((rms - threshold * 0.65) / max(threshold * 0.9, 1e-6), 0.18, 1.0)
        smoothed = np.empty_like(target)
        current = 1.0
        for index, value in enumerate(target):
            coefficient = 0.45 if value > current else 0.12
            current += coefficient * (float(value) - current)
            smoothed[index] = current
        gain_envelope = np.interp(
            np.arange(high_passed.size),
            np.minimum(frame_starts + frame_length // 2, high_passed.size - 1),
            smoothed,
            left=float(smoothed[0]),
            right=float(smoothed[-1]),
        ).astype(np.float32)
        cleaned = high_passed * gain_envelope
        cleaned_peak = float(np.max(np.abs(cleaned)))
        normalize_gain = min(4.0, (10 ** (-1.0 / 20.0)) / max(cleaned_peak, 1e-6))
        cleaned = np.clip(cleaned * normalize_gain, -1.0, 1.0).astype(np.float32)
        cleaned_rms = np.array([
            float(np.sqrt(np.mean(np.square(cleaned[start:start + frame_length], dtype=np.float64)) + 1e-12))
            for start in frame_starts
        ], dtype=np.float32)
        noise_floor_after = float(np.percentile(cleaned_rms, 20))

        temporary = output.with_suffix(".wav.partial")
        save_audio(temporary, cleaned, sample_rate, "wav")
        os.replace(temporary, output)
        processing = {
            "schema_version": 1,
            "algorithm": "soundar-speech-cleanup-v1",
            "sample_rate": sample_rate,
            "high_pass_hz": cutoff_hz,
            "high_pass_order": 2,
            "gate_floor": 0.18,
            "noise_floor_before_dbfs": round(20.0 * math.log10(max(noise_floor_before, 1e-8)), 2),
            "noise_floor_after_dbfs": round(20.0 * math.log10(max(noise_floor_after, 1e-8)), 2),
            "gated_frame_ratio": round(float(np.mean(target < 0.999)), 4),
            "normalization_gain_db": round(20.0 * math.log10(max(normalize_gain, 1e-8)), 2),
            "original_peak_dbfs": round(20.0 * math.log10(max(original_peak, 1e-8)), 2),
        }
        return {"audio_path": str(output), "processing": processing}

    def master_audio(self, request: dict[str, object]) -> dict[str, object]:
        """Build a deterministic, non-destructive long-form master from rendered clips."""
        import numpy as np

        raw_paths = request.get("audio_paths")
        raw_output = str(request.get("output_path", "")).strip()
        if not isinstance(raw_paths, list) or not raw_paths or not raw_output:
            raise ValueError("Mastering requires at least one rendered clip and an output path.")
        paths = [Path(str(value)).expanduser().resolve() for value in raw_paths]
        if len(paths) > 2_000 or any(not path.is_file() for path in paths):
            raise ValueError("One or more rendered project clips are unavailable.")
        output = Path(raw_output).expanduser().resolve()
        if output.suffix.lower() not in {".wav", ".flac"}:
            raise ValueError("Project masters must use WAV or FLAC.")

        sample_rate = int(request.get("sample_rate", 48_000))
        if sample_rate not in {24_000, 44_100, 48_000}:
            raise ValueError("Master sample rate must be 24, 44.1, or 48 kHz.")
        gap_ms = max(0, min(5_000, int(request.get("gap_ms", 250))))
        fade_ms = max(0, min(1_000, int(request.get("fade_ms", 12))))
        target_lufs = max(-24.0, min(-9.0, float(request.get("target_lufs", -16.0))))
        silence_threshold = 10 ** (-50.0 / 20.0)
        fade_samples = round(sample_rate * fade_ms / 1_000)
        gap = np.zeros(round(sample_rate * gap_ms / 1_000), dtype=np.float32)
        clips: list[np.ndarray] = []
        clip_durations: list[float] = []

        for path in paths:
            audio, _ = load_audio(path, target_sr=sample_rate, mono=True)
            audio = np.asarray(audio, dtype=np.float32)
            audible = np.flatnonzero(np.abs(audio) > silence_threshold)
            if audible.size:
                audio = audio[int(audible[0]):int(audible[-1]) + 1]
            if not audio.size:
                continue
            applied_fade = min(fade_samples, audio.size // 2)
            if applied_fade:
                ramp = np.linspace(0.0, 1.0, applied_fade, dtype=np.float32)
                audio[:applied_fade] *= ramp
                audio[-applied_fade:] *= ramp[::-1]
            clips.append(audio)
            clip_durations.append(round(audio.size / sample_rate, 4))
        if not clips:
            raise ValueError("Rendered clips contain no audible samples.")

        pieces: list[np.ndarray] = []
        for index, clip in enumerate(clips):
            if index:
                pieces.append(gap)
            pieces.append(clip)
        master = np.concatenate(pieces).astype(np.float32, copy=False)
        measured_lufs = None
        try:
            import pyloudnorm as pyln

            meter = pyln.Meter(sample_rate)
            measured_lufs = float(meter.integrated_loudness(master))
            if math.isfinite(measured_lufs):
                gain_db = max(-18.0, min(18.0, target_lufs - measured_lufs))
                master *= 10 ** (gain_db / 20.0)
        except Exception:
            rms = float(np.sqrt(np.mean(master ** 2)))
            gain_db = max(-18.0, min(18.0, target_lufs - 20.0 * math.log10(max(rms, 1e-6))))
            master *= 10 ** (gain_db / 20.0)

        # A deterministic soft limiter protects the export after loudness gain.
        ceiling = 10 ** (-1.0 / 20.0)
        peak = float(np.max(np.abs(master)))
        if peak > ceiling:
            master *= ceiling / peak
        master = np.clip(master, -1.0, 1.0).astype(np.float32)
        output.parent.mkdir(parents=True, exist_ok=True)
        temporary = output.with_suffix(output.suffix + ".partial")
        save_audio(temporary, master, sample_rate, output.suffix.lstrip("."))
        os.replace(temporary, output)
        waveform = compute_waveform_envelope(master, max(96, min(480, round(master.size / sample_rate * 8))))
        return {
            "audio_path": str(output),
            "sample_rate": sample_rate,
            "duration_seconds": round(master.size / sample_rate, 4),
            "waveform": [round(float(value), 4) for value in waveform],
            "processing": {
                "schema_version": 1,
                "clip_count": len(clips),
                "clip_durations": clip_durations,
                "trim_threshold_dbfs": -50.0,
                "fade_ms": fade_ms,
                "gap_ms": gap_ms,
                "target_lufs": target_lufs,
                "measured_lufs_before_gain": round(measured_lufs, 2) if measured_lufs is not None and math.isfinite(measured_lufs) else None,
                "peak_ceiling_dbfs": -1.0,
                "sample_rate": sample_rate,
            },
        }

    def transcribe(self, request: dict[str, object]) -> dict[str, object]:
        model_id = str(request.get("model_id", "")).strip()
        raw_path = str(request.get("audio_path", "")).strip()
        if not model_id or not raw_path:
            raise ValueError("Transcription requires an installed model and audio file.")
        installed = self.manager.get_downloaded_model(model_id)
        if installed is None or installed.get("task") != "stt":
            raise ValueError(f"Speech-to-text model is not installed: {model_id}")
        path = Path(raw_path).expanduser()
        if not path.is_file():
            raise ValueError(f"Audio was not found: {path}")

        self.engine.unload_model()
        self.speaker_verifier.unload_model()
        self.forced_aligner.unload_model()
        audio, sample_rate = load_audio(path, target_sr=16_000)
        engine_name = str(installed.get("engine", "transformers"))
        self._validate_scope(engine_name)
        model_path = str(installed.get("local_path", ""))
        self.stt_engine.load_model(model_id, model_path, engine_name)
        collector = BenchmarkCollector(self.gpu)
        collector.start()
        result = self.stt_engine.transcribe(audio, sample_rate)
        metrics = collector.stop(model_id, engine_name, "stt", result.audio_duration_seconds)
        return {
            "model_id": result.model_id,
            "engine": result.engine,
            "text": result.text,
            "segments": [
                {
                    "text": segment.text,
                    "start_seconds": segment.start_seconds,
                    "end_seconds": segment.end_seconds,
                }
                for segment in result.segments
            ],
            "words": [
                {
                    "text": word.text,
                    "start_seconds": word.start_seconds,
                    "end_seconds": word.end_seconds,
                    "confidence": word.confidence,
                    "end_inferred": word.end_inferred,
                }
                for word in result.words
            ],
            "detected_language": result.detected_language,
            "language_confidence": result.language_confidence,
            "evidence": result.evidence,
            "audio_duration_seconds": result.audio_duration_seconds,
            "inference_seconds": result.duration_seconds,
            "rtf": metrics.rtf,
            "vram_peak_mb": metrics.vram_peak_mb,
        }

    def compare_speakers(self, request: dict[str, object]) -> dict[str, object]:
        model_id = str(request.get("model_id", "")).strip()
        reference_path = Path(str(request.get("reference_audio_path", ""))).expanduser()
        candidate_path = Path(str(request.get("candidate_audio_path", ""))).expanduser()
        if not model_id or not reference_path.is_file() or not candidate_path.is_file():
            raise ValueError("Speaker comparison requires an installed verifier and two audio files.")
        installed = self.manager.get_downloaded_model(model_id)
        if installed is None or installed.get("task") != "speaker-verification":
            raise ValueError(f"Speaker-verification model is not installed: {model_id}")

        engine_name = str(installed.get("engine", "speaker-verification"))
        self._validate_scope(engine_name)
        self.engine.unload_model()
        self.stt_engine.unload_model()
        self.forced_aligner.unload_model()
        reference, sample_rate = load_audio(reference_path, target_sr=16_000, mono=True)
        candidate, candidate_rate = load_audio(candidate_path, target_sr=16_000, mono=True)
        if candidate_rate != sample_rate:
            raise ValueError("Speaker comparison audio could not be normalized to one sample rate.")
        self.speaker_verifier.load_model(model_id, str(installed.get("local_path", "")))
        collector = BenchmarkCollector(self.gpu)
        collector.start()
        similarity, inference_seconds = self.speaker_verifier.compare(
            reference, candidate, sample_rate
        )
        metrics = collector.stop(
            model_id,
            engine_name,
            "speaker-verification",
            min(reference.size, candidate.size) / sample_rate,
        )
        return {
            "model_id": model_id,
            "engine": engine_name,
            "similarity": similarity,
            "inference_seconds": inference_seconds,
            "vram_peak_mb": metrics.vram_peak_mb,
            "reference_duration_seconds": reference.size / sample_rate,
            "candidate_duration_seconds": candidate.size / sample_rate,
            "scoring_version": "cosine-normalized-xvector-v1",
        }

    def diarize(self, request: dict[str, object]) -> dict[str, object]:
        model_id = str(request.get("model_id", "")).strip()
        audio_path = Path(str(request.get("audio_path", ""))).expanduser()
        raw_words = request.get("words")
        if not model_id or not audio_path.is_file() or not isinstance(raw_words, list):
            raise ValueError(
                "Speaker separation requires an installed verifier, managed audio, and measured words."
            )
        words = [word for word in raw_words if isinstance(word, dict)]
        if len(words) != len(raw_words):
            raise ValueError("Speaker separation received invalid word evidence.")
        installed = self.manager.get_downloaded_model(model_id)
        if installed is None or installed.get("task") != "speaker-verification":
            raise ValueError(f"Speaker-verification model is not installed: {model_id}")

        requested_count = request.get("speaker_count")
        speaker_count = None
        if requested_count is not None:
            if not isinstance(requested_count, int) or isinstance(requested_count, bool):
                raise ValueError("Speaker count must be a whole number.")
            speaker_count = requested_count
        distance_threshold = float(request.get("distance_threshold", 0.32))
        windows = build_speech_windows(words)
        if speaker_count is not None and speaker_count > len(windows):
            raise ValueError("Speaker count cannot exceed the available speech windows.")

        engine_name = str(installed.get("engine", "speaker-verification"))
        self._validate_scope(engine_name)
        self.engine.unload_model()
        self.stt_engine.unload_model()
        self.forced_aligner.unload_model()
        audio, sample_rate = load_audio(audio_path, target_sr=16_000, mono=True)
        clips = extract_window_audio(audio, sample_rate, windows)
        self.speaker_verifier.load_model(model_id, str(installed.get("local_path", "")))
        collector = BenchmarkCollector(self.gpu)
        collector.start()
        embeddings, inference_seconds = self.speaker_verifier.embed_clips(
            clips, sample_rate
        )
        labels = average_link_cluster(
            embeddings,
            speaker_count=speaker_count,
            distance_threshold=distance_threshold,
        )
        turns = turns_from_windows(words, windows, labels)
        metrics = collector.stop(
            model_id,
            engine_name,
            "speaker-verification",
            audio.size / sample_rate,
        )
        speaker_ids = sorted(
            {str(turn["speaker_id"]) for turn in turns},
            key=lambda value: int(value.rsplit("-", 1)[-1]),
        )
        return {
            "model_id": model_id,
            "engine": engine_name,
            "speakers": [
                {"id": speaker_id, "default_name": f"Speaker {index + 1}"}
                for index, speaker_id in enumerate(speaker_ids)
            ],
            "turns": turns,
            "inference_seconds": inference_seconds,
            "vram_peak_mb": metrics.vram_peak_mb,
            "evidence": {
                "schema_version": 1,
                "method": "wavlm-xvector-word-window-clustering",
                "model_id": model_id,
                "clustering": "average-link-cosine",
                "distance_threshold": distance_threshold,
                "speaker_count_mode": "fixed" if speaker_count is not None else "automatic",
                "requested_speaker_count": speaker_count,
                "speech_window_count": len(windows),
                "target_window_seconds": 2.2,
                "maximum_window_seconds": 3.2,
                "split_gap_seconds": 0.55,
                "embedding_context_seconds": 0.12,
                "overlap_detection": False,
                "confidence_source": "unavailable",
                "provisional": True,
            },
        }

    def align_transcript(self, request: dict[str, object]) -> dict[str, object]:
        model_id = str(request.get("model_id", "")).strip()
        audio_path = Path(str(request.get("audio_path", ""))).expanduser()
        raw_segments = request.get("segments")
        source_revision = request.get("source_revision")
        source_text_sha256 = str(request.get("source_text_sha256", "")).strip()
        if (
            not model_id
            or not audio_path.is_file()
            or not isinstance(raw_segments, list)
            or not isinstance(source_revision, int)
            or source_revision < 0
            or not re.fullmatch(r"[0-9a-f]{64}", source_text_sha256)
        ):
            raise ValueError(
                "Forced alignment requires an installed aligner and revision-linked transcript evidence."
            )
        segments = [dict(segment) for segment in raw_segments if isinstance(segment, dict)]
        if len(segments) != len(raw_segments):
            raise ValueError("Forced alignment received invalid transcript segments.")
        installed = self.manager.get_downloaded_model(model_id)
        if installed is None or installed.get("task") != "alignment":
            raise ValueError(f"Alignment model is not installed: {model_id}")
        engine_name = str(installed.get("engine", "alignment"))
        self._validate_scope(engine_name)
        self.engine.unload_model()
        self.stt_engine.unload_model()
        self.speaker_verifier.unload_model()
        self.forced_aligner.unload_model()
        audio, sample_rate = load_audio(audio_path, target_sr=16_000, mono=True)
        self.forced_aligner.load_model(model_id, str(installed.get("local_path", "")))
        collector = BenchmarkCollector(self.gpu)
        collector.start()
        words, inference_seconds = self.forced_aligner.align(audio, sample_rate, segments)
        metrics = collector.stop(
            model_id, engine_name, "alignment", audio.size / sample_rate
        )
        mean_score = float(
            np.mean([float(word["alignment_score"]) for word in words])
        ) if words else 0.0
        return {
            "model_id": model_id,
            "engine": engine_name,
            "source_revision": source_revision,
            "source_text_sha256": source_text_sha256,
            "words": words,
            "mean_alignment_score": mean_score,
            "inference_seconds": inference_seconds,
            "vram_peak_mb": metrics.vram_peak_mb,
            "evidence": {
                "schema_version": 1,
                "method": "ctc-viterbi-segment-alignment",
                "language": "en",
                "source_revision_linked": True,
                "score_source": "mean-ctc-token-path-probability",
                "score_calibrated": False,
                "original_timestamps_preserved": True,
                "provisional": True,
            },
        }

    def synthesize(self, request: dict[str, object]) -> dict[str, object]:
        text = str(request.get("text", "")).strip()
        model_id = str(request.get("model_id", "")).strip()
        output_format = str(request.get("output_format", "wav")).lower()
        if not text:
            raise ValueError("The script is empty.")
        if output_format not in {"wav", "flac"}:
            raise ValueError("Output format must be wav or flac.")
        if str(request.get("input_mode", "text")) == "ssml":
            text = normalize_basic_ssml(text)

        installed = self.manager.get_downloaded_model(model_id)
        if installed is None:
            raise ValueError(f"Model is not installed: {model_id}")

        engine_name = str(installed.get("engine", self.manager.detect_engine(model_id)))
        self._validate_scope(engine_name)
        language = self.contracts.validate_synthesis(engine_name, request)
        model_path = str(installed.get("local_path", ""))
        self.stt_engine.unload_model()
        self.speaker_verifier.unload_model()
        self.engine.load_model(model_id, model_path, engine_name)

        reference_audio = None
        reference_sr = None
        reference_path = request.get("reference_audio_path")
        if reference_path:
            path = Path(str(reference_path)).expanduser()
            if not path.is_file():
                raise ValueError(f"Reference audio was not found: {path}")
            reference_audio, reference_sr = load_audio(path, target_sr=24_000)

        collector = BenchmarkCollector(self.gpu)
        collector.start()
        preview_segments: list[np.ndarray] = []
        preview_started = time.monotonic()
        preview_path: Path | None = None
        job_id = str(request.get("_job_id") or "").strip()
        if engine_name == "fish-speech" and self.event_sink is not None and re.fullmatch(r"[A-Za-z0-9_-]{8,80}", job_id):
            preview_path = Path.home() / ".soundAr" / "exports" / f".preview-{job_id}.wav"

        def publish_preview(segment: np.ndarray, sample_rate: int) -> None:
            if preview_path is None or self.event_sink is None or segment.size == 0:
                return
            preview_segments.append(np.asarray(segment, dtype=np.float32).reshape(-1))
            combined = np.concatenate(preview_segments)
            temporary = preview_path.with_suffix(".wav.partial")
            save_audio(temporary, combined, sample_rate, "wav")
            os.replace(temporary, preview_path)
            sequence = len(preview_segments)
            self.event_sink({
                "type": "audio-preview",
                "stage": "decoding",
                "progress": min(0.92, 0.78 + sequence * 0.02),
                "audio_path": str(preview_path),
                "duration_seconds": round(combined.size / sample_rate, 4),
                "first_audio_seconds": round(time.monotonic() - preview_started, 4),
                "sequence": sequence,
            })

        result = self.engine.synthesize(
            text=text,
            speaker=str(request.get("speaker") or "default"),
            language=language,
            reference_audio=reference_audio,
            reference_sr=reference_sr,
            controls={
                "speed": float(request.get("speed", 1.0)),
                "exaggeration": float(request.get("exaggeration", 0.5)),
                "cfg_weight": float(request.get("cfg_weight", 0.5)),
                "temperature": float(request.get("temperature", 0.8)),
                "top_p": float(request.get("top_p", 0.95)),
                "repetition_penalty": float(request.get("repetition_penalty", 1.2)),
                "cfg_scale": float(request.get("cfg_scale", 1.0)),
                "instruction": str(request.get("instruction") or "Speak clearly and naturally."),
                "seed": int(request.get("seed", 0)),
            },
            progress_callback=publish_preview if preview_path is not None else None,
        )
        metrics = collector.stop(model_id, engine_name, "tts", result.duration_seconds)

        return self._write_generated_audio(
            request=request,
            model_id=model_id,
            engine_name=engine_name,
            audio=result.audio,
            sample_rate=result.sample_rate,
            duration_seconds=result.duration_seconds,
            metrics=metrics,
            generation_kind="speech",
        )

    def generate_music(self, request: dict[str, object]) -> dict[str, object]:
        """Generate a short local music draft from a text prompt."""
        prompt = str(request.get("prompt", "")).strip()
        lyrics = str(request.get("lyrics") or "").strip()
        model_id = str(request.get("model_id", "")).strip()
        installed = self.manager.get_downloaded_model(model_id)
        if installed is None:
            raise ValueError(f"Model is not installed: {model_id}")
        if str(installed.get("task", "")) != "music":
            raise ValueError(f"{model_id} is not a music-generation model.")

        engine_name = str(installed.get("engine", self.manager.detect_engine(model_id)))
        self._validate_scope(engine_name)
        self.contracts.validate_music_generation(engine_name, request)
        model_path = str(installed.get("local_path", ""))
        manifest_controls = self.contracts.get(engine_name).get("controls", {})
        duration_seconds = float(
            request.get(
                "duration_seconds",
                manifest_controls.get("duration_seconds", {}).get("default", 10.0),
            )
        )
        integer_controls = {"top_k", "inference_steps", "bpm"}
        controls: dict[str, float | int] = {"seed": int(request.get("seed", 0))}
        for name in manifest_controls:
            if name not in request:
                continue
            controls[name] = (
                int(request[name]) if name in integer_controls else float(request[name])
            )
        vocal_language = (
            self.contracts.normalize_language(
                engine_name, str(request.get("vocal_language") or "en")
            )
            if lyrics
            else None
        )
        advanced_fields = (
            "mode",
            "quality_profile",
            "planner_enabled",
            "reference_audio_path",
            "source_audio_path",
            "repainting_start",
            "repainting_end",
            "audio_cover_strength",
            "key_scale",
            "time_signature",
            "stem_type",
            "return_lyric_timing",
            "return_stems",
            "parent_history_id",
        )
        advanced = {
            name: request[name]
            for name in advanced_fields
            if name in request and request[name] is not None
        }

        self.engine.unload_model()
        self.stt_engine.unload_model()
        self.speaker_verifier.unload_model()
        self.forced_aligner.unload_model()
        self.music_engine.load_model(model_id, model_path, engine_name)
        collector = BenchmarkCollector(self.gpu)
        collector.start()
        result = self.music_engine.generate(
            prompt=prompt,
            duration_seconds=duration_seconds,
            controls=controls,
            lyrics=lyrics or None,
            vocal_language=vocal_language,
            advanced=advanced,
        )
        metrics = collector.stop(model_id, engine_name, "music", result.duration_seconds)
        return self._write_generated_audio(
            request=request,
            model_id=model_id,
            engine_name=engine_name,
            audio=result.audio,
            sample_rate=result.sample_rate,
            duration_seconds=result.duration_seconds,
            metrics=metrics,
            generation_kind="music",
        )

    def _write_generated_audio(
        self,
        *,
        request: dict[str, object],
        model_id: str,
        engine_name: str,
        audio: np.ndarray,
        sample_rate: int,
        duration_seconds: float,
        metrics: object,
        generation_kind: str,
    ) -> dict[str, object]:
        """Stage an artifact for native checksum verification and atomic publication."""
        output_format = str(request.get("output_format", "wav")).lower()
        export_dir = Path.home() / ".soundAr" / "exports"
        export_dir.mkdir(parents=True, exist_ok=True)
        result_id = uuid.uuid4().hex
        output_name = str(request.get("output_name") or "").strip()
        if output_name:
            if len(output_name) > 96 or not all(
                character.isascii() and (character.isalnum() or character in "-_")
                for character in output_name
            ):
                raise ValueError("Output name may contain only ASCII letters, numbers, hyphens, and underscores")
            filename = f"{output_name}.{output_format}"
        else:
            prefix = "soundar-music" if generation_kind == "music" else "soundar"
            filename = f"{prefix}-{datetime.now().strftime('%Y%m%d-%H%M%S')}-{result_id[:6]}.{output_format}"
        output_path = export_dir / filename
        temporary_path = output_path.with_suffix(output_path.suffix + ".partial")
        try:
            save_audio(temporary_path, audio, sample_rate, output_format)
        except Exception:
            temporary_path.unlink(missing_ok=True)
            raise
        waveform_bins = max(48, min(240, round(duration_seconds * 14)))
        waveform = compute_waveform_envelope(audio, waveform_bins)
        return {
            "id": result_id,
            "model_id": model_id,
            "engine": engine_name,
            "generation_kind": generation_kind,
            "audio_path": str(output_path),
            "staging_path": str(temporary_path),
            "sample_rate": sample_rate,
            "duration_seconds": duration_seconds,
            "inference_seconds": getattr(metrics, "inference_seconds"),
            "rtf": getattr(metrics, "rtf"),
            "vram_peak_mb": getattr(metrics, "vram_peak_mb"),
            "waveform": [round(float(value), 4) for value in waveform],
            "created_at": datetime.now(timezone.utc).isoformat(),
            "preview": False,
        }

    def _validate_scope(self, engine: str) -> None:
        if self.engine_scope not in {"foundation", engine}:
            raise RuntimeError(
                f"Worker for '{self.engine_scope}' cannot execute the '{engine}' engine."
            )


def normalize_basic_ssml(source: str) -> str:
    """Normalize the safe SSML subset supported consistently by local engines."""
    try:
        root = ET.fromstring(source)
    except ET.ParseError as error:
        raise ValueError(f"Invalid SSML: {error}") from error
    if root.tag != "speak":
        raise ValueError("SSML must use a single <speak> root element.")
    allowed = {"speak", "p", "s", "break"}
    unsupported = sorted({element.tag for element in root.iter() if element.tag not in allowed})
    if unsupported:
        raise ValueError(
            "This engine-independent SSML mode does not support: "
            + ", ".join(f"<{tag}>" for tag in unsupported)
        )

    parts: list[str] = []

    def visit(element: ET.Element) -> None:
        if element.text and element.text.strip():
            parts.append(element.text.strip())
        for child in element:
            if child.tag == "break":
                time_value = child.attrib.get("time", "").strip().lower()
                strength = child.attrib.get("strength", "").strip().lower()
                if time_value and not re.fullmatch(r"\d+(?:\.\d+)?(?:ms|s)", time_value):
                    raise ValueError("SSML break time must use milliseconds or seconds.")
                break_seconds = 0.0
                if time_value.endswith("ms"):
                    break_seconds = float(time_value[:-2]) / 1000.0
                elif time_value.endswith("s"):
                    break_seconds = float(time_value[:-1])
                parts.append("." if strength in {"strong", "x-strong"} or break_seconds >= 0.7 else ",")
            else:
                visit(child)
                if child.tag in {"p", "s"}:
                    parts.append(".")
            if child.tail and child.tail.strip():
                parts.append(child.tail.strip())

    visit(root)
    normalized = " ".join(parts)
    normalized = " ".join(normalized.split())
    if not normalized.strip(" .,;:"):
        raise ValueError("SSML contains no speakable text.")
    return normalized


def analyze_audio_path(path: Path) -> dict[str, object]:
    info = inspect_audio(path)
    audio, sample_rate = load_audio_raw(path)
    absolute = abs(audio)
    peak = float(absolute.max()) if absolute.size else 0.0
    rms = float(__import__("numpy").sqrt(__import__("numpy").mean(audio ** 2))) if audio.size else 0.0
    peak_dbfs = 20.0 * float(__import__("math").log10(max(peak, 1e-6)))
    rms_dbfs = 20.0 * float(__import__("math").log10(max(rms, 1e-6)))
    silence_ratio = float((absolute < 0.01).mean()) if absolute.size else 1.0
    clipping_ratio = float((absolute >= 0.999).mean()) if absolute.size else 0.0
    waveform = compute_waveform_envelope(audio, 120)
    return {
        "format": info.format,
        "sample_rate": sample_rate,
        "channels": info.channels,
        "duration_seconds": info.duration_seconds,
        "peak_dbfs": round(peak_dbfs, 2),
        "rms_dbfs": round(rms_dbfs, 2),
        "silence_ratio": round(silence_ratio, 4),
        "clipping_ratio": round(clipping_ratio, 6),
        "waveform": [round(float(value), 4) for value in waveform],
        "warnings": [
            message
            for condition, message in (
                (info.duration_seconds < 3.0, "Reference is shorter than 3 seconds."),
                (info.duration_seconds > 120.0, "Reference is longer than 2 minutes."),
                (silence_ratio > 0.45, "Reference contains substantial silence."),
                (clipping_ratio > 0.001, "Reference contains clipped samples."),
                (sample_rate < 16_000, "Reference sample rate is below 16 kHz."),
            )
            if condition
        ],
    }


def serve(runtime: Runtime) -> int:
    protocol_stdout = sys.stdout
    for line in sys.stdin:
        try:
            request = json.loads(line)
            runtime.event_sink = lambda event: print(
                json.dumps({"event": event}),
                file=protocol_stdout,
                flush=True,
            )
            with contextlib.redirect_stdout(io.StringIO()):
                result = runtime.dispatch(request)
            response = {"ok": True, "result": result}
        except Exception as error:
            response = {"ok": False, "error": str(error)}
        finally:
            runtime.event_sink = None
        print(json.dumps(response), file=protocol_stdout, flush=True)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--request")
    parser.add_argument("--serve", action="store_true")
    args = parser.parse_args()
    runtime = Runtime()
    if args.serve:
        return serve(runtime)
    if not args.request:
        parser.error("--request is required unless --serve is used")
    try:
        request = json.loads(args.request)
        # Model libraries can be noisy. Keep stdout reserved for the response JSON.
        with contextlib.redirect_stdout(io.StringIO()):
            response = runtime.dispatch(request)
        print(json.dumps(response))
        return 0
    except Exception as error:
        print(str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
