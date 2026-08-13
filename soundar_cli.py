#!/usr/bin/env python3
"""Dependency-free client for the explicitly started soundAr local API."""
from __future__ import annotations

import argparse
import csv
import json
import os
import sys
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path


def load_batch_rows(path: Path) -> list[dict[str, object]]:
    suffix = path.suffix.lower()
    if suffix == ".txt":
        rows = [{"text": line.strip()} for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
    elif suffix == ".jsonl":
        rows = []
        for index, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
            if not line.strip():
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError as error:
                raise RuntimeError(f"JSONL row {index} is invalid: {error}") from error
            if isinstance(row, str):
                row = {"text": row}
            if not isinstance(row, dict):
                raise RuntimeError(f"JSONL row {index} must be a string or object")
            rows.append(row)
    elif suffix == ".csv":
        with path.open(encoding="utf-8", newline="") as handle:
            reader = csv.DictReader(handle)
            if not reader.fieldnames or "text" not in {name.strip().lower() for name in reader.fieldnames}:
                raise RuntimeError("CSV batch imports require a text column")
            rows = []
            for index, source in enumerate(reader, start=2):
                normalized = {str(key).strip().lower(): str(value or "").strip() for key, value in source.items()}
                settings: dict[str, object] = {}
                for key in ("model_id", "speaker", "language", "output_format"):
                    if normalized.get(key):
                        settings[key] = normalized[key]
                for key in ("speed", "exaggeration", "cfg_weight", "temperature", "top_p", "repetition_penalty"):
                    if normalized.get(key):
                        try:
                            settings[key] = float(normalized[key])
                        except ValueError as error:
                            raise RuntimeError(f"CSV row {index} has an invalid {key}") from error
                if normalized.get("seed"):
                    try:
                        settings["seed"] = int(normalized["seed"])
                    except ValueError as error:
                        raise RuntimeError(f"CSV row {index} has an invalid seed") from error
                row = {"text": normalized.get("text", ""), "name": normalized.get("name", ""), "output_name": normalized.get("output_name", ""), "settings": settings}
                if normalized.get("priority"):
                    row["priority"] = normalized["priority"]
                rows.append(row)
    else:
        raise RuntimeError("Batch input must be TXT, CSV, or JSONL")
    if not rows:
        raise RuntimeError("Batch input contains no rows")
    if len(rows) > 1_000:
        raise RuntimeError("Batch input cannot contain more than 1,000 rows")
    return rows


def request(
    base_url: str,
    token: str,
    path: str,
    payload: dict[str, object] | None = None,
    extra_headers: dict[str, str] | None = None,
) -> tuple[bytes, str]:
    body = json.dumps(payload).encode() if payload is not None else None
    headers = {"Authorization": f"Bearer {token}"}
    if body is not None:
        headers["Content-Type"] = "application/json"
    if extra_headers:
        headers.update(extra_headers)
    operation = urllib.request.Request(base_url.rstrip("/") + path, data=body, headers=headers)
    try:
        with urllib.request.urlopen(operation, timeout=1_900) as response:
            return response.read(), response.headers.get_content_type()
    except urllib.error.HTTPError as error:
        detail = error.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"soundAr API returned HTTP {error.code}: {detail}") from error
    except urllib.error.URLError as error:
        raise RuntimeError(f"Could not reach soundAr at {base_url}: {error.reason}") from error


def main() -> int:
    parser = argparse.ArgumentParser(prog="soundar", description="Client for the soundAr local speech API")
    parser.add_argument("--base-url", default=os.environ.get("SOUNDAR_API_URL", "http://127.0.0.1:17843"))
    parser.add_argument("--token", default=os.environ.get("SOUNDAR_API_TOKEN"))
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("status")
    commands.add_parser("models")
    commands.add_parser("capabilities")
    commands.add_parser("voices")
    commands.add_parser("jobs")
    job = commands.add_parser("job")
    job.add_argument("job_id")
    commands.add_parser("scheduler")
    commands.add_parser("history")
    commands.add_parser("batches")
    commands.add_parser("benchmarks")
    cancel = commands.add_parser("cancel")
    cancel.add_argument("job_id")
    retry = commands.add_parser("retry")
    retry.add_argument("job_id")
    commands.add_parser("clear-finished")
    pause_batch = commands.add_parser("pause-batch")
    pause_batch.add_argument("batch_id")
    cancel_batch = commands.add_parser("cancel-batch")
    cancel_batch.add_argument("batch_id")
    resume_batch = commands.add_parser("resume-batch")
    resume_batch.add_argument("batch_id")
    resume_batch.add_argument("--parallelism", type=int, choices=range(1, 9), default=2)
    resume_batch.add_argument("--retry-failed", action="store_true")
    batch = commands.add_parser("batch")
    batch.add_argument("input", type=Path, help="UTF-8 TXT, CSV, or JSONL batch file")
    batch.add_argument("--name", default="CLI batch")
    batch.add_argument("--model", default="hexgrad/Kokoro-82M")
    batch.add_argument("--voice", default="af_heart")
    batch.add_argument("--language", default="en")
    batch.add_argument("--format", choices=("wav", "flac"), default="wav")
    batch.add_argument("--parallelism", type=int, choices=range(1, 9), default=2)
    batch.add_argument("--priority", choices=("low", "normal", "high", "urgent"), default="normal")
    batch.add_argument("--idempotency-key")
    batch.add_argument("--no-wait", action="store_true", help="Return after the batch is accepted")
    generate = commands.add_parser("generate")
    generate.add_argument("text")
    generate.add_argument("--model", default="hexgrad/Kokoro-82M")
    generate.add_argument("--voice", default="af_heart")
    generate.add_argument("--language", default="en")
    generate.add_argument("--speed", type=float, default=1.0)
    generate.add_argument("--seed", type=int, default=42817)
    generate.add_argument("--format", choices=("wav", "flac"), default="wav")
    generate.add_argument("--priority", choices=("low", "normal", "high", "urgent"), default="normal")
    generate.add_argument("--output", type=Path, required=True)
    queue = commands.add_parser("queue")
    queue.add_argument("text")
    queue.add_argument("--model", default="hexgrad/Kokoro-82M")
    queue.add_argument("--voice", default="af_heart")
    queue.add_argument("--language", default="en")
    queue.add_argument("--speed", type=float, default=1.0)
    queue.add_argument("--seed", type=int, default=42817)
    queue.add_argument("--format", choices=("wav", "flac"), default="wav")
    queue.add_argument("--idempotency-key")
    queue.add_argument("--priority", choices=("low", "normal", "high", "urgent"), default="normal")
    queue.add_argument("--no-wait", action="store_true")
    queue.add_argument("--output", type=Path)
    args = parser.parse_args()
    if not args.token:
        parser.error("set SOUNDAR_API_TOKEN or pass --token after starting the API in soundAr Settings")

    try:
        if args.command == "status":
            body, _ = request(args.base_url, args.token, "/health")
            print(json.dumps(json.loads(body), indent=2))
        elif args.command in {"models", "capabilities", "voices", "jobs", "history", "batches", "benchmarks", "scheduler"}:
            path = "/v1/runtime/scheduler" if args.command == "scheduler" else f"/v1/{args.command}"
            body, _ = request(args.base_url, args.token, path)
            print(json.dumps(json.loads(body), indent=2))
        elif args.command == "job":
            body, _ = request(args.base_url, args.token, f"/v1/jobs/{args.job_id}")
            print(json.dumps(json.loads(body), indent=2))
        elif args.command == "cancel":
            body, _ = request(args.base_url, args.token, f"/v1/jobs/{args.job_id}/cancel", {})
            print(json.dumps(json.loads(body), indent=2))
        elif args.command == "retry":
            body, _ = request(args.base_url, args.token, f"/v1/jobs/{args.job_id}/retry", {})
            print(json.dumps(json.loads(body), indent=2))
        elif args.command == "clear-finished":
            body, _ = request(args.base_url, args.token, "/v1/jobs/clear-finished", {})
            print(json.dumps(json.loads(body), indent=2))
        elif args.command == "pause-batch":
            body, _ = request(args.base_url, args.token, f"/v1/batches/{args.batch_id}/pause", {})
            print(json.dumps(json.loads(body), indent=2))
        elif args.command == "cancel-batch":
            body, _ = request(args.base_url, args.token, f"/v1/batches/{args.batch_id}/cancel", {})
            print(json.dumps(json.loads(body), indent=2))
        elif args.command == "resume-batch":
            body, _ = request(args.base_url, args.token, f"/v1/batches/{args.batch_id}/resume", {
                "parallelism": args.parallelism,
                "retry_failed": args.retry_failed,
            })
            print(json.dumps(json.loads(body), indent=2))
        elif args.command == "batch":
            rows = load_batch_rows(args.input)
            body, _ = request(
                args.base_url,
                args.token,
                "/v1/batches",
                {
                    "name": args.name,
                    "rows": rows,
                    "parallelism": args.parallelism,
                    "priority": args.priority,
                    "settings": {
                        "model_id": args.model,
                        "speaker": args.voice,
                        "language": args.language,
                        "output_format": args.format,
                        "speed": 1.0,
                        "seed": 42817,
                    },
                },
                {"Idempotency-Key": args.idempotency_key or uuid.uuid4().hex},
            )
            batch_result = json.loads(body)
            if not args.no_wait:
                while batch_result.get("status") in {"queued", "running"}:
                    time.sleep(0.5)
                    body, _ = request(args.base_url, args.token, f"/v1/batches/{batch_result['id']}")
                    batch_result = json.loads(body)
            print(json.dumps(batch_result, indent=2))
            if batch_result.get("status") == "failed":
                return 2
        elif args.command == "generate":
            body, media_type = request(args.base_url, args.token, "/v1/audio/speech", {
                "model": args.model, "input": args.text, "voice": args.voice,
                "language": args.language, "speed": args.speed, "seed": args.seed,
                "response_format": args.format,
                "priority": args.priority,
            })
            expected = b"RIFF" if args.format == "wav" else b"fLaC"
            if not body.startswith(expected):
                raise RuntimeError(f"soundAr returned invalid {args.format.upper()} data ({media_type})")
            args.output.parent.mkdir(parents=True, exist_ok=True)
            temporary = args.output.with_suffix(args.output.suffix + ".partial")
            temporary.write_bytes(body)
            temporary.replace(args.output)
            print(args.output.resolve())
        elif args.command == "queue":
            idempotency_key = args.idempotency_key or uuid.uuid4().hex
            body, _ = request(
                args.base_url,
                args.token,
                "/v1/audio/speech/jobs",
                {
                    "model": args.model, "input": args.text, "voice": args.voice,
                    "language": args.language, "speed": args.speed, "seed": args.seed,
                    "response_format": args.format,
                    "priority": args.priority,
                },
                {"Idempotency-Key": idempotency_key},
            )
            job_result = json.loads(body)
            if not args.no_wait:
                while job_result.get("status") in {"queued", "preparing", "running"}:
                    time.sleep(0.25)
                    body, _ = request(args.base_url, args.token, f"/v1/jobs/{job_result['id']}")
                    job_result = json.loads(body)
            if args.output and job_result.get("status") == "completed":
                audio, media_type = request(
                    args.base_url, args.token, f"/v1/jobs/{job_result['id']}/audio"
                )
                expected = b"RIFF" if args.format == "wav" else b"fLaC"
                if not audio.startswith(expected):
                    raise RuntimeError(f"soundAr returned invalid {args.format.upper()} data ({media_type})")
                args.output.parent.mkdir(parents=True, exist_ok=True)
                temporary = args.output.with_suffix(args.output.suffix + ".partial")
                temporary.write_bytes(audio)
                temporary.replace(args.output)
                job_result["downloaded_to"] = str(args.output.resolve())
            print(json.dumps(job_result, indent=2))
            if job_result.get("status") in {"failed", "cancelled"}:
                return 2
        else:
            raise RuntimeError(f"Unsupported command: {args.command}")
        return 0
    except RuntimeError as error:
        print(error, file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
