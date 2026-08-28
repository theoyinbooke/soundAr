//! Headless control-plane entry point for the packaged soundAr binary.
//!
//! The CLI intentionally owns no production workflow. It parses one strict request, creates the
//! same `RuntimeState` used by Tauri, and dispatches through the authenticated Codex video-tool
//! adapter. This keeps GUI, assistant, and headless behavior on one application service.

use super::{
    codex_agent::{
        self, VideoAgentOperation, VideoAgentResult, VideoAgentResultStatus, VideoAgentToolError,
    },
    project_root, python_path, RuntimeState,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    env,
    ffi::OsString,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

const MAX_AGENT_REQUEST_BYTES: u64 = 1_048_576;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputStyle {
    Compact,
    Pretty,
}

#[derive(Debug, Eq, PartialEq)]
enum AgentCommand {
    Help,
    Tools {
        style: OutputStyle,
    },
    Video {
        tool: String,
        request: Option<String>,
        style: OutputStyle,
        progress: bool,
    },
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct AgentEnvelope<T: Serialize> {
    schema_version: u16,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<VideoAgentToolError>,
}

impl<T: Serialize> AgentEnvelope<T> {
    fn success(result: T) -> Self {
        Self {
            schema_version: 1,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    fn failure(error: VideoAgentToolError) -> Self {
        Self {
            schema_version: 1,
            ok: false,
            result: None,
            error: Some(error),
        }
    }
}

pub(super) fn run(arguments: Vec<OsString>) -> i32 {
    let command = match parse_arguments(arguments) {
        Ok(command) => command,
        Err(error) => {
            write_error(&error, OutputStyle::Compact);
            return 2;
        }
    };
    match command {
        AgentCommand::Help => {
            print_help();
            0
        }
        AgentCommand::Tools { style } => {
            write_stdout(
                &AgentEnvelope::success(codex_agent::video_dynamic_tools()),
                style,
            );
            0
        }
        AgentCommand::Video {
            tool,
            request,
            style,
            progress,
        } => run_video(tool, request, style, progress),
    }
}

fn run_video(
    tool: String,
    inline_request: Option<String>,
    style: OutputStyle,
    progress: bool,
) -> i32 {
    let request = match read_request(inline_request) {
        Ok(request) => request,
        Err(error) => {
            write_error(&error, style);
            return 2;
        }
    };
    let operation = match VideoAgentOperation::parse(&tool, request) {
        Ok(operation) => operation,
        Err(error) => {
            let exit = exit_code(&error);
            write_error(&error, style);
            return exit;
        }
    };
    let runtime = match RuntimeState::new(headless_runtime_root(), python_path()) {
        Ok(runtime) => runtime,
        Err(message) => {
            let error = VideoAgentToolError::from(format!("video.runtime_unavailable: {message}"));
            write_error(&error, style);
            return exit_code(&error);
        }
    };
    let callback = progress.then(|| {
        Arc::new(|event: super::video::VideoServiceProgress| {
            let envelope = json!({
                "schema_version": 1,
                "type": "progress",
                "event": event,
            });
            let _ = writeln!(io::stderr().lock(), "{envelope}");
        }) as super::video::ProgressCallback
    });
    let dispatched = match operation {
        VideoAgentOperation::RegisterGeneratedVisual(request) => {
            let thread_id = request
                .thread_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    VideoAgentToolError::new(
                        "video.generation_identity_required",
                        "Headless generation registration requires thread_id",
                    )
                });
            thread_id.and_then(|thread_id| {
                codex_agent::resolve_headless_generated_visual(thread_id, &request.generation_id)
                    .and_then(|generation| {
                        runtime
                            .video
                            .register_trusted_generated_visual(
                                super::video::AuthorizeVisualSelectionRequest {
                                    project_id: request.project_id.clone(),
                                    expected_revision: request.expected_revision,
                                    expected_version_id: request.expected_version_id,
                                },
                                generation,
                            )
                            .map_err(VideoAgentToolError::from)
                    })
                    .and_then(|receipt| {
                        serde_json::to_value(receipt)
                            .map_err(|error| {
                                VideoAgentToolError::new(
                                    "video.agent_result_encode_failed",
                                    format!("Could not encode the generation receipt: {error}"),
                                )
                            })
                            .map(|data| {
                                VideoAgentResult::project_data(
                                    super::codex_agent::VideoAgentOperationKind::RegisterGeneratedVisual,
                                    "Authenticated Codex generation registered; use this one-use receipt with add_visual_asset",
                                    request.project_id,
                                    data,
                                )
                            })
                    })
            })
        }
        operation => {
            super::video_commands::dispatch_video_operation(&runtime, operation, callback)
        }
    }
    .and_then(|result| settle_queued_result(&runtime, result));
    let exit = match dispatched {
        Ok(result) => {
            write_stdout(&AgentEnvelope::success(result), style);
            0
        }
        Err(error) => {
            let exit = exit_code(&error);
            write_error(&error, style);
            exit
        }
    };
    let _ = runtime.stop_active_worker();
    exit
}

fn settle_queued_result(
    runtime: &RuntimeState,
    result: VideoAgentResult,
) -> Result<VideoAgentResult, VideoAgentToolError> {
    if result.status != VideoAgentResultStatus::Queued {
        return Ok(result);
    }
    let project_id = result.project_id.as_deref().ok_or_else(|| {
        VideoAgentToolError::from(
            "video.invalid_job_result: A queued headless job has no project identifier",
        )
    })?;
    let job_id = result.job_id.as_deref().ok_or_else(|| {
        VideoAgentToolError::from(
            "video.invalid_job_result: A queued headless job has no durable identifier",
        )
    })?;
    let completed = runtime
        .video
        .wait_for_job(job_id, project_id, Duration::from_secs(6 * 60 * 60))
        .map_err(VideoAgentToolError::from)?;
    let project = super::video::present_video_project(
        &completed.project,
        &runtime.store.video_artifacts_root(),
    )
    .map_err(VideoAgentToolError::from)?;
    VideoAgentResult::project(
        result.operation,
        VideoAgentResultStatus::Completed,
        format!("{}; durable job completed", result.summary),
        project,
        Some(job_id.to_string()),
    )
}

fn parse_arguments(arguments: Vec<OsString>) -> Result<AgentCommand, VideoAgentToolError> {
    let arguments = arguments
        .into_iter()
        .map(|value| {
            value
                .into_string()
                .map_err(|_| cli_error("Arguments must be valid UTF-8 text"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let Some(command) = arguments.first().map(String::as_str) else {
        return Ok(AgentCommand::Help);
    };
    if matches!(command, "help" | "--help" | "-h") {
        return Ok(AgentCommand::Help);
    }
    let style = if arguments.iter().any(|argument| argument == "--pretty") {
        OutputStyle::Pretty
    } else {
        OutputStyle::Compact
    };
    match command {
        "tools" => {
            reject_unknown_flags(&arguments[1..], &["--pretty"])?;
            Ok(AgentCommand::Tools { style })
        }
        "video" => {
            let tool = arguments
                .get(1)
                .filter(|value| !value.starts_with('-'))
                .cloned()
                .ok_or_else(|| cli_error("video requires a tool name"))?;
            let mut request = None;
            let mut progress = false;
            let mut index = 2;
            while index < arguments.len() {
                match arguments[index].as_str() {
                    "--pretty" => index += 1,
                    "--progress" => {
                        progress = true;
                        index += 1;
                    }
                    "--request" => {
                        let value = arguments
                            .get(index + 1)
                            .cloned()
                            .ok_or_else(|| cli_error("--request requires one JSON object"))?;
                        if request.replace(value).is_some() {
                            return Err(cli_error("--request may be supplied only once"));
                        }
                        index += 2;
                    }
                    unknown => return Err(cli_error(format!("Unknown agent option: {unknown}"))),
                }
            }
            Ok(AgentCommand::Video {
                tool,
                request,
                style,
                progress,
            })
        }
        _ => Err(cli_error(format!("Unknown agent command: {command}"))),
    }
}

fn reject_unknown_flags(arguments: &[String], allowed: &[&str]) -> Result<(), VideoAgentToolError> {
    if let Some(argument) = arguments
        .iter()
        .find(|argument| !allowed.contains(&argument.as_str()))
    {
        return Err(cli_error(format!("Unknown agent option: {argument}")));
    }
    Ok(())
}

fn read_request(inline: Option<String>) -> Result<Value, VideoAgentToolError> {
    let text = match inline {
        Some(value) => {
            if value.len() as u64 > MAX_AGENT_REQUEST_BYTES {
                return Err(cli_error("Agent request exceeds the 1 MiB limit"));
            }
            value
        }
        None => {
            let mut bytes = Vec::new();
            io::stdin()
                .take(MAX_AGENT_REQUEST_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| cli_error(format!("Could not read the agent request: {error}")))?;
            if bytes.len() as u64 > MAX_AGENT_REQUEST_BYTES {
                return Err(cli_error("Agent request exceeds the 1 MiB limit"));
            }
            String::from_utf8(bytes)
                .map_err(|_| cli_error("Agent request must be valid UTF-8 JSON"))?
        }
    };
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_str(&text)
        .map_err(|error| cli_error(format!("Agent request is not valid JSON: {error}")))?;
    if !value.is_object() {
        return Err(cli_error("Agent request must be one JSON object"));
    }
    Ok(value)
}

fn headless_runtime_root() -> PathBuf {
    resolve_headless_runtime_root(
        env::var_os("SOUNDAR_RUNTIME_ROOT"),
        env::current_exe().ok(),
        cfg!(debug_assertions),
    )
}

fn resolve_headless_runtime_root(
    configured: Option<OsString>,
    executable: Option<PathBuf>,
    development: bool,
) -> PathBuf {
    if let Some(path) = configured {
        return PathBuf::from(path);
    }
    if development {
        return project_root();
    }
    let mut candidates = Vec::new();
    if let Some(executable) = executable {
        if let Some(bin) = executable.parent() {
            candidates.push(bin.join("../lib/soundAr/runtime"));
            candidates.push(bin.join("../Resources/runtime"));
            candidates.push(bin.join("runtime"));
        }
    }
    candidates.push(PathBuf::from("/usr/lib/soundAr/runtime"));
    candidates
        .into_iter()
        .find(|candidate| runtime_marker(candidate).is_file())
        .and_then(|candidate| candidate.canonicalize().ok().or(Some(candidate)))
        .unwrap_or_else(project_root)
}

fn runtime_marker(root: &Path) -> PathBuf {
    root.join("bridge.py")
}

fn cli_error(message: impl Into<String>) -> VideoAgentToolError {
    VideoAgentToolError::from(format!("video.agent_bad_arguments: {}", message.into()))
}

fn exit_code(error: &VideoAgentToolError) -> i32 {
    if error.approval_required {
        3
    } else if error.retryable {
        4
    } else if error.code.contains("invalid") || error.code.contains("bad_arguments") {
        2
    } else {
        5
    }
}

fn write_error(error: &VideoAgentToolError, style: OutputStyle) {
    write_stdout(&AgentEnvelope::<Value>::failure(error.clone()), style);
}

fn write_stdout(value: &impl Serialize, style: OutputStyle) {
    let serialized = match style {
        OutputStyle::Compact => serde_json::to_string(value),
        OutputStyle::Pretty => serde_json::to_string_pretty(value),
    }
    .unwrap_or_else(|error| {
        json!({
            "schema_version": 1,
            "ok": false,
            "error": {
                "code": "video.agent_serialization_failed",
                "message": error.to_string(),
                "retryable": false,
                "approval_required": false
            }
        })
        .to_string()
    });
    let _ = writeln!(io::stdout().lock(), "{serialized}");
}

fn print_help() {
    println!(
        "soundAr headless agent\n\n\
         Usage:\n  soundar-desktop agent tools [--pretty]\n  \
         soundar-desktop agent video <tool> [--request '{{...}}'] [--progress] [--pretty]\n\n\
         Without --request, the video command reads one JSON object (up to 1 MiB) from stdin.\n\
         Final result JSON is written to stdout. Optional progress JSON lines are written to stderr."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parser_is_strict_and_stdin_safe_by_default() {
        assert_eq!(parse_arguments(vec![]).unwrap(), AgentCommand::Help);
        assert_eq!(
            parse_arguments(vec!["tools".into(), "--pretty".into()]).unwrap(),
            AgentCommand::Tools {
                style: OutputStyle::Pretty
            }
        );
        assert_eq!(
            parse_arguments(vec![
                "video".into(),
                "list_video_projects".into(),
                "--progress".into(),
            ])
            .unwrap(),
            AgentCommand::Video {
                tool: "list_video_projects".to_string(),
                request: None,
                style: OutputStyle::Compact,
                progress: true,
            }
        );
        assert!(parse_arguments(vec!["video".into()]).is_err());
        assert!(parse_arguments(vec!["tools".into(), "--unknown".into()]).is_err());
        assert!(parse_arguments(vec![
            "video".into(),
            "get_video_project".into(),
            "--request".into(),
            "{}".into(),
            "--request".into(),
            "{}".into(),
        ])
        .is_err());
    }

    #[test]
    fn runtime_root_honors_explicit_configuration() {
        assert_eq!(
            resolve_headless_runtime_root(
                Some(OsString::from("/tmp/soundar-agent-runtime-test")),
                None,
                false,
            ),
            PathBuf::from("/tmp/soundar-agent-runtime-test")
        );
    }

    #[test]
    fn exit_codes_preserve_approval_and_retry_boundaries() {
        let mut approval = VideoAgentToolError::from("video.rights_required: confirm rights");
        approval.approval_required = true;
        assert_eq!(exit_code(&approval), 3);
        let mut retryable = VideoAgentToolError::from("video.runtime_busy: retry later");
        retryable.retryable = true;
        assert_eq!(exit_code(&retryable), 4);
        assert_eq!(exit_code(&cli_error("bad")), 2);
        assert_eq!(
            exit_code(&VideoAgentToolError::from("video.render_failed: failed")),
            5
        );
    }
}
