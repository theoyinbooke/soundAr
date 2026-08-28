#!/usr/bin/env bash

# Shared safety and validation helpers for the Linux Video Studio harness.
# This file is sourced by the executable scripts; it intentionally performs no
# work on its own.

video_die() {
  printf 'video-studio: %s\n' "$*" >&2
  exit 1
}

video_require_command() {
  local command_name="$1"
  command -v -- "$command_name" >/dev/null 2>&1 \
    || video_die "required command not found: $command_name"
}

video_resolve_executable() {
  local configured_path="$1"
  shift

  if [[ -n "$configured_path" ]]; then
    [[ -f "$configured_path" && -x "$configured_path" ]] \
      || video_die "configured executable is not an executable file: $configured_path"
    realpath -- "$configured_path"
    return 0
  fi

  local candidate
  for candidate in "$@"; do
    if command -v -- "$candidate" >/dev/null 2>&1; then
      command -v -- "$candidate"
      return 0
    fi
  done
  return 1
}

video_prepare_output_dir() {
  local requested="$1"
  [[ -n "$requested" ]] || video_die 'output directory must not be empty'
  [[ "$requested" != '/' && "$requested" != '.' && "$requested" != '..' ]] \
    || video_die "refusing unsafe output directory: $requested"

  if [[ -L "$requested" ]]; then
    video_die "output directory must not be a symbolic link: $requested"
  fi
  mkdir -p -- "$requested"
  [[ -d "$requested" ]] || video_die "could not create output directory: $requested"
  realpath -- "$requested"
}

video_sha256() {
  local input_path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -- "$input_path" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -- "$input_path" | awk '{print $1}'
  else
    video_die 'sha256sum or shasum is required'
  fi
}

video_validate_media() {
  local ffprobe_path="$1"
  local input_path="$2"
  [[ -s "$input_path" ]] || video_die "media file is empty or missing: $input_path"

  "$ffprobe_path" \
    -v error \
    -select_streams v:0 \
    -show_entries stream=codec_name,width,height,duration \
    -of csv=p=0 \
    -- "$input_path" >/dev/null \
    || video_die "ffprobe rejected media file: $input_path"
}

video_cleanup_staging_dir() {
  local staging_dir="${1:-}"
  [[ -n "$staging_dir" && -d "$staging_dir" ]] || return 0
  case "$(basename -- "$staging_dir")" in
    .fixture-stage.*|.render-stage.*|.video-test.*)
      if ! rmdir -- "$staging_dir" 2>/dev/null; then
        printf 'video-studio: preserving non-empty staging directory: %s\n' "$staging_dir" >&2
      fi
      ;;
    *)
      printf 'video-studio: preserving unrecognized staging directory: %s\n' "$staging_dir" >&2
      ;;
  esac
}
