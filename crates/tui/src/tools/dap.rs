//! Debug Adapter Protocol tool surface.
//!
//! This is intentionally a small first slice: one supported adapter
//! (`debugpy`) and a bounded set of DAP requests that cover breakpoints,
//! stepping, stack frames, scopes, and variables.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_bool, optional_str, optional_u64, required_str,
};
use crate::utils::spawn_supervised;

const DEFAULT_WAIT_MS: u64 = 3_000;
const REQUEST_WAIT_MS: u64 = 10_000;
const DEFAULT_STACK_LEVELS: u64 = 20;
const DEFAULT_VARIABLE_COUNT: u64 = 100;

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);
static SESSION_STORE: std::sync::OnceLock<AsyncMutex<HashMap<String, Arc<DapSession>>>> =
    std::sync::OnceLock::new();

pub struct DapDebuggerTool;

#[async_trait]
impl ToolSpec for DapDebuggerTool {
    fn name(&self) -> &'static str {
        "dap_debugger"
    }

    fn description(&self) -> &'static str {
        "Start or attach to a supported Debug Adapter Protocol session and inspect breakpoints, threads, stack frames, scopes, and variables. First slice supports debugpy only; launched programs and cwd are resolved through the workspace policy."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": [
                        "start", "attach", "status", "stop",
                        "set_breakpoints", "list_breakpoints", "clear_breakpoints",
                        "continue", "next", "step_in", "step_out", "pause",
                        "threads", "stack", "scopes", "variables"
                    ],
                    "description": "Debugger operation to run."
                },
                "session_id": {
                    "type": "string",
                    "description": "Debugger session id returned by start/attach. Omit only when exactly one session is active."
                },
                "adapter": {
                    "type": "string",
                    "enum": ["debugpy"],
                    "description": "Supported DAP adapter. Defaults to debugpy."
                },
                "program": {
                    "type": "string",
                    "description": "start/debugpy only: workspace-relative Python program to launch."
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "start/debugpy only: program argv strings."
                },
                "cwd": {
                    "type": "string",
                    "description": "start/debugpy only: workspace-relative working directory. Defaults to workspace root."
                },
                "stop_on_entry": {
                    "type": "boolean",
                    "description": "start/debugpy only: request a stop on entry before user code runs."
                },
                "host": {
                    "type": "string",
                    "description": "attach/debugpy only: localhost adapter target host. Defaults to 127.0.0.1."
                },
                "port": {
                    "type": "integer",
                    "description": "attach/debugpy only: debugpy listen port."
                },
                "path": {
                    "type": "string",
                    "description": "Breakpoint source path for set_breakpoints/clear_breakpoints."
                },
                "lines": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "Breakpoint line numbers for set_breakpoints."
                },
                "thread_id": {
                    "type": "integer",
                    "description": "Thread id for stepping/stack operations. Omitted means first adapter thread."
                },
                "frame_id": {
                    "type": "integer",
                    "description": "Frame id for scopes."
                },
                "variables_reference": {
                    "type": "integer",
                    "description": "DAP variablesReference for variables."
                },
                "levels": {
                    "type": "integer",
                    "description": "Maximum stack frames to return. Defaults to 20."
                },
                "count": {
                    "type": "integer",
                    "description": "Maximum variables to return. Defaults to 100."
                },
                "wait_ms": {
                    "type": "integer",
                    "description": "Milliseconds to wait for a stopped/terminated event after resume operations. Defaults to 3000."
                }
            },
            "required": ["operation"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::RequiresApproval,
            ToolCapability::Sandboxable,
        ]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        execute_dap_tool(input, context, session_store()).await
    }
}

async fn execute_dap_tool(
    input: Value,
    context: &ToolContext,
    store: &AsyncMutex<HashMap<String, Arc<DapSession>>>,
) -> Result<ToolResult, ToolError> {
    let operation = required_str(&input, "operation")?;
    let namespace = context.state_namespace.clone();

    let value = match operation {
        "start" => start_session(&input, context, store).await?,
        "attach" => attach_session(&input, context, store).await?,
        "status" => status_sessions(store, &namespace).await,
        "stop" => stop_session(&input, context, store).await?,
        "set_breakpoints" => set_breakpoints(&input, context, store).await?,
        "list_breakpoints" => list_breakpoints(&input, context, store).await?,
        "clear_breakpoints" => clear_breakpoints(&input, context, store).await?,
        "continue" | "next" | "step_in" | "step_out" | "pause" => {
            resume_or_step(operation, &input, context, store).await?
        }
        "threads" => threads(&input, context, store).await?,
        "stack" => stack(&input, context, store).await?,
        "scopes" => scopes(&input, context, store).await?,
        "variables" => variables(&input, context, store).await?,
        other => {
            return Err(ToolError::invalid_input(format!(
                "unknown dap_debugger operation `{other}`"
            )));
        }
    };

    ToolResult::json(&value).map_err(|err| ToolError::execution_failed(err.to_string()))
}

async fn start_session(
    input: &Value,
    context: &ToolContext,
    store: &AsyncMutex<HashMap<String, Arc<DapSession>>>,
) -> Result<Value, ToolError> {
    let adapter = adapter_name(input)?;
    let program_arg = required_str(input, "program")?;
    let program = context.resolve_path(program_arg)?;
    if !program.is_file() {
        return Err(ToolError::execution_failed(format!(
            "debug program must be an existing file inside the workspace: {}",
            program.display()
        )));
    }
    let cwd = resolve_cwd(input, context)?;
    let args = string_array(input, "args")?;
    let stop_on_entry = optional_bool(input, "stop_on_entry", false);
    let breakpoints = breakpoint_lines(input)?;
    let transport = spawn_supported_adapter(adapter).await?;

    let session = initialize_session(
        adapter,
        transport,
        context.state_namespace.clone(),
        "launch",
    )
    .await?;

    let launch_args = json!({
        "program": program.display().to_string(),
        "cwd": cwd.display().to_string(),
        "args": args,
        "stopOnEntry": stop_on_entry,
        "console": "internalConsole",
        "justMyCode": false
    });
    let launch_pending = session
        .transport
        .request_detached("launch", launch_args)
        .await?;
    let initialized = session
        .transport
        .wait_event(&["initialized"], Duration::from_millis(DEFAULT_WAIT_MS))
        .await
        .ok();

    let mut configured_breakpoints = Vec::new();
    if !breakpoints.is_empty() {
        configured_breakpoints = send_set_breakpoints(&session, &program, &breakpoints).await?;
    }
    let configuration_done = session
        .transport
        .request(
            "configurationDone",
            json!({}),
            Duration::from_millis(REQUEST_WAIT_MS),
        )
        .await?;
    let launch = launch_pending
        .wait(Duration::from_millis(REQUEST_WAIT_MS))
        .await?;
    let event = session
        .transport
        .wait_event(&["stopped", "terminated", "exited"], wait_duration(input))
        .await
        .ok();

    let id = session.id.clone();
    store
        .lock()
        .await
        .insert(store_key(&session.namespace, &id), session);

    Ok(json!({
        "session_id": id,
        "adapter": adapter,
        "mode": "launch",
        "program": program.display().to_string(),
        "initialize": "ok",
        "launch": launch,
        "initialized_event": initialized,
        "configurationDone": configuration_done,
        "breakpoints": configured_breakpoints,
        "event": event,
        "supported_adapters": ["debugpy"]
    }))
}

async fn attach_session(
    input: &Value,
    context: &ToolContext,
    store: &AsyncMutex<HashMap<String, Arc<DapSession>>>,
) -> Result<Value, ToolError> {
    let adapter = adapter_name(input)?;
    let host = optional_str(input, "host").unwrap_or("127.0.0.1");
    if !matches!(host, "127.0.0.1" | "localhost" | "::1") {
        return Err(ToolError::invalid_input(
            "debugpy attach is limited to localhost targets in this first slice".to_string(),
        ));
    }
    let port = required_positive_u64(input, "port")?;
    if port > u16::MAX as u64 {
        return Err(ToolError::invalid_input(
            "port must fit in the TCP port range".to_string(),
        ));
    }

    let transport = spawn_supported_adapter(adapter).await?;
    let session = initialize_session(
        adapter,
        transport,
        context.state_namespace.clone(),
        "attach",
    )
    .await?;
    let attach_pending = session
        .transport
        .request_detached(
            "attach",
            json!({ "connect": { "host": host, "port": port } }),
        )
        .await?;
    let initialized = session
        .transport
        .wait_event(&["initialized"], Duration::from_millis(DEFAULT_WAIT_MS))
        .await
        .ok();
    let configuration_done = session
        .transport
        .request(
            "configurationDone",
            json!({}),
            Duration::from_millis(REQUEST_WAIT_MS),
        )
        .await?;
    let attach = attach_pending
        .wait(Duration::from_millis(REQUEST_WAIT_MS))
        .await?;
    let id = session.id.clone();
    store
        .lock()
        .await
        .insert(store_key(&session.namespace, &id), session);

    Ok(json!({
        "session_id": id,
        "adapter": adapter,
        "mode": "attach",
        "target": { "host": host, "port": port },
        "attach": attach,
        "initialized_event": initialized,
        "configurationDone": configuration_done
    }))
}

async fn initialize_session(
    adapter: &'static str,
    transport: Arc<dyn DapTransport>,
    namespace: String,
    mode: &'static str,
) -> Result<Arc<DapSession>, ToolError> {
    let id = format!("dap-{}", NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed));
    let initialize = transport
        .request(
            "initialize",
            json!({
                "clientID": "codewhale",
                "clientName": "CodeWhale",
                "adapterID": adapter,
                "pathFormat": "path",
                "linesStartAt1": true,
                "columnsStartAt1": true,
                "supportsVariableType": true,
                "supportsVariablePaging": true,
                "supportsRunInTerminalRequest": false
            }),
            Duration::from_millis(REQUEST_WAIT_MS),
        )
        .await?;
    Ok(Arc::new(DapSession {
        id,
        namespace,
        adapter,
        mode,
        initialize,
        transport,
        breakpoints: Arc::new(AsyncMutex::new(HashMap::new())),
    }))
}

async fn set_breakpoints(
    input: &Value,
    context: &ToolContext,
    store: &AsyncMutex<HashMap<String, Arc<DapSession>>>,
) -> Result<Value, ToolError> {
    let session = find_session(input, context, store).await?;
    let path_arg = required_str(input, "path")?;
    let path = context.resolve_path(path_arg)?;
    let lines = breakpoint_lines(input)?;
    let breakpoints = send_set_breakpoints(&session, &path, &lines).await?;
    Ok(json!({
        "session_id": session.id,
        "path": path.display().to_string(),
        "breakpoints": breakpoints
    }))
}

async fn send_set_breakpoints(
    session: &DapSession,
    path: &Path,
    lines: &[u64],
) -> Result<Vec<Value>, ToolError> {
    let requested: Vec<Value> = lines.iter().map(|line| json!({ "line": line })).collect();
    let body = session
        .transport
        .request(
            "setBreakpoints",
            json!({
                "source": { "path": path.display().to_string() },
                "breakpoints": requested,
                "lines": lines
            }),
            Duration::from_millis(REQUEST_WAIT_MS),
        )
        .await?;
    let adapter_breakpoints = body
        .get("breakpoints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let verified_lines = adapter_breakpoints
        .iter()
        .filter(|bp| bp.get("verified").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|bp| bp.get("line").and_then(Value::as_u64))
        .collect::<Vec<_>>();
    session
        .breakpoints
        .lock()
        .await
        .insert(path.to_path_buf(), verified_lines);
    Ok(adapter_breakpoints)
}

async fn list_breakpoints(
    input: &Value,
    context: &ToolContext,
    store: &AsyncMutex<HashMap<String, Arc<DapSession>>>,
) -> Result<Value, ToolError> {
    let session = find_session(input, context, store).await?;
    let breakpoints = session
        .breakpoints
        .lock()
        .await
        .iter()
        .map(|(path, lines)| json!({ "path": path.display().to_string(), "lines": lines }))
        .collect::<Vec<_>>();
    Ok(json!({ "session_id": session.id, "breakpoints": breakpoints }))
}

async fn clear_breakpoints(
    input: &Value,
    context: &ToolContext,
    store: &AsyncMutex<HashMap<String, Arc<DapSession>>>,
) -> Result<Value, ToolError> {
    let session = find_session(input, context, store).await?;
    if let Some(path_arg) = optional_str(input, "path") {
        let path = context.resolve_path(path_arg)?;
        let breakpoints = send_set_breakpoints(&session, &path, &[]).await?;
        session.breakpoints.lock().await.remove(&path);
        Ok(json!({
            "session_id": session.id,
            "cleared": [{ "path": path.display().to_string() }],
            "adapter_breakpoints": breakpoints
        }))
    } else {
        let paths = session
            .breakpoints
            .lock()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut cleared = Vec::new();
        for path in paths {
            let _ = send_set_breakpoints(&session, &path, &[]).await?;
            cleared.push(json!({ "path": path.display().to_string() }));
        }
        session.breakpoints.lock().await.clear();
        Ok(json!({ "session_id": session.id, "cleared": cleared }))
    }
}

async fn resume_or_step(
    operation: &str,
    input: &Value,
    context: &ToolContext,
    store: &AsyncMutex<HashMap<String, Arc<DapSession>>>,
) -> Result<Value, ToolError> {
    let session = find_session(input, context, store).await?;
    let command = match operation {
        "continue" => "continue",
        "next" => "next",
        "step_in" => "stepIn",
        "step_out" => "stepOut",
        "pause" => "pause",
        _ => unreachable!("validated by caller"),
    };
    let thread_id = thread_id_or_first(input, &session).await?;
    let body = session
        .transport
        .request(
            command,
            json!({ "threadId": thread_id }),
            Duration::from_millis(REQUEST_WAIT_MS),
        )
        .await?;
    let event = session
        .transport
        .wait_event(&["stopped", "terminated", "exited"], wait_duration(input))
        .await
        .ok();
    Ok(json!({
        "session_id": session.id,
        "operation": operation,
        "thread_id": thread_id,
        "response": body,
        "event": event
    }))
}

async fn threads(
    input: &Value,
    context: &ToolContext,
    store: &AsyncMutex<HashMap<String, Arc<DapSession>>>,
) -> Result<Value, ToolError> {
    let session = find_session(input, context, store).await?;
    let body = session
        .transport
        .request("threads", json!({}), Duration::from_millis(REQUEST_WAIT_MS))
        .await?;
    Ok(
        json!({ "session_id": session.id, "threads": body.get("threads").cloned().unwrap_or(json!([])) }),
    )
}

async fn stack(
    input: &Value,
    context: &ToolContext,
    store: &AsyncMutex<HashMap<String, Arc<DapSession>>>,
) -> Result<Value, ToolError> {
    let session = find_session(input, context, store).await?;
    let thread_id = thread_id_or_first(input, &session).await?;
    let levels = optional_u64(input, "levels", DEFAULT_STACK_LEVELS);
    let body = session
        .transport
        .request(
            "stackTrace",
            json!({ "threadId": thread_id, "startFrame": 0, "levels": levels }),
            Duration::from_millis(REQUEST_WAIT_MS),
        )
        .await?;
    Ok(
        json!({ "session_id": session.id, "thread_id": thread_id, "stackFrames": body.get("stackFrames").cloned().unwrap_or(json!([])), "totalFrames": body.get("totalFrames").cloned() }),
    )
}

async fn scopes(
    input: &Value,
    context: &ToolContext,
    store: &AsyncMutex<HashMap<String, Arc<DapSession>>>,
) -> Result<Value, ToolError> {
    let session = find_session(input, context, store).await?;
    let frame_id = required_positive_u64(input, "frame_id")?;
    let body = session
        .transport
        .request(
            "scopes",
            json!({ "frameId": frame_id }),
            Duration::from_millis(REQUEST_WAIT_MS),
        )
        .await?;
    Ok(
        json!({ "session_id": session.id, "frame_id": frame_id, "scopes": body.get("scopes").cloned().unwrap_or(json!([])) }),
    )
}

async fn variables(
    input: &Value,
    context: &ToolContext,
    store: &AsyncMutex<HashMap<String, Arc<DapSession>>>,
) -> Result<Value, ToolError> {
    let session = find_session(input, context, store).await?;
    let variables_reference = required_positive_u64(input, "variables_reference")?;
    let count = optional_u64(input, "count", DEFAULT_VARIABLE_COUNT);
    let body = session
        .transport
        .request(
            "variables",
            json!({ "variablesReference": variables_reference, "start": 0, "count": count }),
            Duration::from_millis(REQUEST_WAIT_MS),
        )
        .await?;
    Ok(
        json!({ "session_id": session.id, "variablesReference": variables_reference, "variables": body.get("variables").cloned().unwrap_or(json!([])) }),
    )
}

async fn stop_session(
    input: &Value,
    context: &ToolContext,
    store: &AsyncMutex<HashMap<String, Arc<DapSession>>>,
) -> Result<Value, ToolError> {
    let session = find_session(input, context, store).await?;
    let key = store_key(&context.state_namespace, &session.id);
    let removed = store.lock().await.remove(&key);
    if let Some(session) = removed {
        let response = session
            .transport
            .request(
                "disconnect",
                json!({ "terminateDebuggee": true, "restart": false }),
                Duration::from_millis(DEFAULT_WAIT_MS),
            )
            .await
            .ok();
        session.transport.shutdown().await;
        Ok(json!({ "session_id": session.id, "stopped": true, "disconnect": response }))
    } else {
        Ok(json!({ "session_id": session.id, "stopped": false }))
    }
}

async fn status_sessions(
    store: &AsyncMutex<HashMap<String, Arc<DapSession>>>,
    namespace: &str,
) -> Value {
    let prefix = format!("{namespace}:");
    let sessions = store
        .lock()
        .await
        .iter()
        .filter(|(key, _)| key.starts_with(&prefix))
        .map(|(_, session)| {
            json!({
                "session_id": session.id,
                "adapter": session.adapter,
                "mode": session.mode,
                "initialize": session.initialize
            })
        })
        .collect::<Vec<_>>();
    json!({ "sessions": sessions, "supported_adapters": ["debugpy"] })
}

async fn thread_id_or_first(input: &Value, session: &DapSession) -> Result<u64, ToolError> {
    if let Some(id) = input.get("thread_id").and_then(Value::as_u64) {
        if id == 0 {
            return Err(ToolError::invalid_input(
                "thread_id must be greater than 0".to_string(),
            ));
        }
        return Ok(id);
    }
    let body = session
        .transport
        .request("threads", json!({}), Duration::from_millis(REQUEST_WAIT_MS))
        .await?;
    body.get("threads")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|thread| thread.get("id"))
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            ToolError::execution_failed(
                "adapter returned no threads; pass thread_id after dap_debugger threads"
                    .to_string(),
            )
        })
}

async fn find_session(
    input: &Value,
    context: &ToolContext,
    store: &AsyncMutex<HashMap<String, Arc<DapSession>>>,
) -> Result<Arc<DapSession>, ToolError> {
    let namespace = &context.state_namespace;
    let sessions = store.lock().await;
    if let Some(session_id) = optional_str(input, "session_id") {
        let key = store_key(namespace, session_id);
        return sessions.get(&key).cloned().ok_or_else(|| {
            ToolError::execution_failed(format!(
                "no active dap_debugger session `{session_id}` in this workspace"
            ))
        });
    }
    let prefix = format!("{namespace}:");
    let matches = sessions
        .iter()
        .filter(|(key, _)| key.starts_with(&prefix))
        .map(|(_, session)| session.clone())
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [session] => Ok(session.clone()),
        [] => Err(ToolError::execution_failed(
            "no active dap_debugger sessions; call operation=start or attach first".to_string(),
        )),
        many => Err(ToolError::execution_failed(format!(
            "multiple dap_debugger sessions are active ({}); pass session_id",
            many.len()
        ))),
    }
}

fn session_store() -> &'static AsyncMutex<HashMap<String, Arc<DapSession>>> {
    SESSION_STORE.get_or_init(|| AsyncMutex::new(HashMap::new()))
}

fn store_key(namespace: &str, session_id: &str) -> String {
    format!("{namespace}:{session_id}")
}

fn adapter_name(input: &Value) -> Result<&'static str, ToolError> {
    match optional_str(input, "adapter").unwrap_or("debugpy") {
        "debugpy" => Ok("debugpy"),
        other => Err(ToolError::invalid_input(format!(
            "unsupported DAP adapter `{other}`; supported adapters: debugpy"
        ))),
    }
}

async fn spawn_supported_adapter(adapter: &str) -> Result<Arc<dyn DapTransport>, ToolError> {
    match adapter {
        "debugpy" => {
            let python = std::env::var("CODEWHALE_DEBUGPY_PYTHON")
                .or_else(|_| std::env::var("PYTHON"))
                .unwrap_or_else(|_| "python3".to_string());
            let args = vec!["-m".to_string(), "debugpy.adapter".to_string()];
            let transport = StdioDapTransport::spawn(&python, &args).await.map_err(|err| {
                ToolError::execution_failed(format!(
                    "failed to start debugpy adapter via `{python} -m debugpy.adapter`: {err}. Install with `python3 -m pip install debugpy` or set CODEWHALE_DEBUGPY_PYTHON."
                ))
            })?;
            Ok(Arc::new(transport))
        }
        _ => Err(ToolError::invalid_input(
            "unsupported DAP adapter; supported adapters: debugpy".to_string(),
        )),
    }
}

fn resolve_cwd(input: &Value, context: &ToolContext) -> Result<PathBuf, ToolError> {
    let cwd = if let Some(cwd) = optional_str(input, "cwd") {
        context.resolve_path(cwd)?
    } else {
        context.workspace.clone()
    };
    if !cwd.is_dir() {
        return Err(ToolError::execution_failed(format!(
            "debug cwd must be an existing directory: {}",
            cwd.display()
        )));
    }
    Ok(cwd)
}

fn string_array(input: &Value, key: &str) -> Result<Vec<String>, ToolError> {
    let Some(value) = input.get(key) else {
        return Ok(Vec::new());
    };
    let items = value
        .as_array()
        .ok_or_else(|| ToolError::invalid_input(format!("`{key}` must be an array of strings")))?;
    items
        .iter()
        .map(|item| {
            item.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                ToolError::invalid_input(format!("`{key}` must be an array of strings"))
            })
        })
        .collect()
}

fn breakpoint_lines(input: &Value) -> Result<Vec<u64>, ToolError> {
    let Some(value) = input.get("lines") else {
        return Ok(Vec::new());
    };
    let items = value.as_array().ok_or_else(|| {
        ToolError::invalid_input("`lines` must be an array of positive integers".to_string())
    })?;
    let mut lines = Vec::with_capacity(items.len());
    for item in items {
        let line = item.as_u64().ok_or_else(|| {
            ToolError::invalid_input("`lines` must be an array of positive integers".to_string())
        })?;
        if line == 0 {
            return Err(ToolError::invalid_input(
                "`lines` must contain 1-based positive integers".to_string(),
            ));
        }
        lines.push(line);
    }
    lines.sort_unstable();
    lines.dedup();
    Ok(lines)
}

fn required_positive_u64(input: &Value, key: &str) -> Result<u64, ToolError> {
    let value = input
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| ToolError::invalid_input(format!("missing required integer `{key}`")))?;
    if value == 0 {
        return Err(ToolError::invalid_input(format!(
            "`{key}` must be greater than 0"
        )));
    }
    Ok(value)
}

fn wait_duration(input: &Value) -> Duration {
    Duration::from_millis(optional_u64(input, "wait_ms", DEFAULT_WAIT_MS))
}

#[derive(Clone)]
struct DapSession {
    id: String,
    namespace: String,
    adapter: &'static str,
    mode: &'static str,
    initialize: Value,
    transport: Arc<dyn DapTransport>,
    breakpoints: Arc<AsyncMutex<HashMap<PathBuf, Vec<u64>>>>,
}

#[async_trait]
trait DapTransport: Send + Sync {
    async fn request(
        &self,
        command: &str,
        arguments: Value,
        wait: Duration,
    ) -> Result<Value, ToolError>;
    async fn request_detached(
        &self,
        command: &str,
        arguments: Value,
    ) -> Result<PendingDapRequest, ToolError>;
    async fn wait_event(&self, names: &[&str], wait: Duration) -> Result<Value, ToolError>;
    async fn shutdown(&self);
}

struct PendingDapRequest {
    command: String,
    rx: oneshot::Receiver<Value>,
}

impl PendingDapRequest {
    async fn wait(self, wait: Duration) -> Result<Value, ToolError> {
        let response = timeout(wait, self.rx)
            .await
            .map_err(|_| ToolError::Timeout {
                seconds: wait.as_secs().max(1),
            })?
            .map_err(|_| {
                ToolError::execution_failed(format!(
                    "DAP adapter closed before responding to `{}`",
                    self.command
                ))
            })?;
        response_body(&self.command, response)
    }
}

struct StdioDapTransport {
    child: AsyncMutex<Option<Child>>,
    tx_outbound: mpsc::Sender<Vec<u8>>,
    events_rx: AsyncMutex<mpsc::Receiver<Value>>,
    pending: Arc<AsyncMutex<HashMap<i64, oneshot::Sender<Value>>>>,
    next_seq: AsyncMutex<i64>,
}

impl StdioDapTransport {
    async fn spawn(command: &str, args: &[String]) -> Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn DAP adapter `{command}`"))?;
        let stdin = child
            .stdin
            .take()
            .context("DAP child has no stdin handle")?;
        let stdout = child
            .stdout
            .take()
            .context("DAP child has no stdout handle")?;

        let (tx_outbound, rx_outbound) = mpsc::channel::<Vec<u8>>(64);
        let (tx_inbound, rx_inbound) = mpsc::channel::<Value>(64);
        let (tx_events, rx_events) = mpsc::channel::<Value>(64);
        let pending = Arc::new(AsyncMutex::new(HashMap::new()));

        spawn_supervised(
            "dap-writer",
            std::panic::Location::caller(),
            writer_task(stdin, rx_outbound),
        );
        spawn_supervised(
            "dap-reader",
            std::panic::Location::caller(),
            reader_task(stdout, tx_inbound),
        );
        spawn_supervised(
            "dap-dispatcher",
            std::panic::Location::caller(),
            dispatcher_task(rx_inbound, tx_events, pending.clone()),
        );

        Ok(Self {
            child: AsyncMutex::new(Some(child)),
            tx_outbound,
            events_rx: AsyncMutex::new(rx_events),
            pending,
            next_seq: AsyncMutex::new(1),
        })
    }
}

#[async_trait]
impl DapTransport for StdioDapTransport {
    async fn request(
        &self,
        command: &str,
        arguments: Value,
        wait: Duration,
    ) -> Result<Value, ToolError> {
        self.request_detached(command, arguments)
            .await?
            .wait(wait)
            .await
    }

    async fn request_detached(
        &self,
        command: &str,
        arguments: Value,
    ) -> Result<PendingDapRequest, ToolError> {
        let mut seq = self.next_seq.lock().await;
        let request_seq = *seq;
        *seq += 1;
        drop(seq);

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(request_seq, tx);

        let request = json!({
            "seq": request_seq,
            "type": "request",
            "command": command,
            "arguments": arguments
        });
        if let Err(err) = send_message(&self.tx_outbound, &request).await {
            self.pending.lock().await.remove(&request_seq);
            return Err(ToolError::execution_failed(err.to_string()));
        }
        Ok(PendingDapRequest {
            command: command.to_string(),
            rx,
        })
    }

    async fn wait_event(&self, names: &[&str], wait: Duration) -> Result<Value, ToolError> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(ToolError::Timeout {
                    seconds: wait.as_secs().max(1),
                });
            }
            let remaining = deadline - now;
            let mut rx = self.events_rx.lock().await;
            let event = match timeout(remaining, rx.recv()).await {
                Ok(Some(event)) => event,
                Ok(None) => {
                    return Err(ToolError::execution_failed(
                        "DAP event channel closed".to_string(),
                    ));
                }
                Err(_) => {
                    return Err(ToolError::Timeout {
                        seconds: wait.as_secs().max(1),
                    });
                }
            };
            drop(rx);
            let name = event.get("event").and_then(Value::as_str);
            if name.is_some_and(|name| names.contains(&name)) {
                return Ok(event);
            }
        }
    }

    async fn shutdown(&self) {
        let mut child = self.child.lock().await;
        if let Some(mut child) = child.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

fn response_body(command: &str, response: Value) -> Result<Value, ToolError> {
    let success = response
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if success {
        Ok(response.get("body").cloned().unwrap_or_else(|| json!({})))
    } else {
        let message = response
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| {
                response
                    .get("body")
                    .and_then(|body| body.get("error"))
                    .and_then(|err| err.get("format"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("adapter returned an unsuccessful response");
        Err(ToolError::execution_failed(format!(
            "DAP `{command}` failed: {message}"
        )))
    }
}

async fn send_message(tx: &mpsc::Sender<Vec<u8>>, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value).context("serialize DAP message")?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut frame = Vec::with_capacity(header.len() + body.len());
    frame.extend_from_slice(header.as_bytes());
    frame.extend_from_slice(&body);
    tx.send(frame)
        .await
        .map_err(|_| anyhow!("DAP outbound channel closed"))?;
    Ok(())
}

async fn writer_task(mut stdin: tokio::process::ChildStdin, mut rx: mpsc::Receiver<Vec<u8>>) {
    while let Some(frame) = rx.recv().await {
        if stdin.write_all(&frame).await.is_err() {
            break;
        }
        if stdin.flush().await.is_err() {
            break;
        }
    }
}

async fn reader_task(mut stdout: tokio::process::ChildStdout, tx: mpsc::Sender<Value>) {
    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut tmp = [0u8; 4096];
    loop {
        let n = match stdout.read(&mut tmp).await {
            Ok(0) => return,
            Ok(n) => n,
            Err(_) => return,
        };
        buf.extend_from_slice(&tmp[..n]);
        while let Some((header_end, content_length)) = parse_header(&buf) {
            if buf.len() < header_end + content_length {
                break;
            }
            let body = &buf[header_end..header_end + content_length];
            let parsed = serde_json::from_slice::<Value>(body).ok();
            buf.drain(..header_end + content_length);
            if let Some(value) = parsed
                && tx.send(value).await.is_err()
            {
                return;
            }
        }
    }
}

async fn dispatcher_task(
    mut rx: mpsc::Receiver<Value>,
    tx_events: mpsc::Sender<Value>,
    pending: Arc<AsyncMutex<HashMap<i64, oneshot::Sender<Value>>>>,
) {
    while let Some(value) = rx.recv().await {
        match value.get("type").and_then(Value::as_str) {
            Some("event") => {
                let _ = tx_events.send(value).await;
            }
            Some("response") => {
                if let Some(request_seq) = value.get("request_seq").and_then(Value::as_i64) {
                    let mut map = pending.lock().await;
                    if let Some(slot) = map.remove(&request_seq) {
                        let _ = slot.send(value);
                    }
                }
            }
            _ => {}
        }
    }
}

fn parse_header(buf: &[u8]) -> Option<(usize, usize)> {
    let term = b"\r\n\r\n";
    let pos = buf.windows(term.len()).position(|window| window == term)?;
    let header = std::str::from_utf8(&buf[..pos]).ok()?;
    let mut content_length: Option<usize> = None;
    for line in header.split("\r\n") {
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse::<usize>().ok();
        }
    }
    content_length.map(|len| (pos + term.len(), len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    struct FakeDapTransport {
        requests: AsyncMutex<Vec<(String, Value)>>,
        events: AsyncMutex<Vec<Value>>,
    }

    impl FakeDapTransport {
        fn new(events: Vec<Value>) -> Self {
            Self {
                requests: AsyncMutex::new(Vec::new()),
                events: AsyncMutex::new(events),
            }
        }
    }

    #[async_trait]
    impl DapTransport for FakeDapTransport {
        async fn request(
            &self,
            command: &str,
            arguments: Value,
            wait: Duration,
        ) -> Result<Value, ToolError> {
            self.request_detached(command, arguments)
                .await?
                .wait(wait)
                .await
        }

        async fn request_detached(
            &self,
            command: &str,
            arguments: Value,
        ) -> Result<PendingDapRequest, ToolError> {
            self.requests
                .lock()
                .await
                .push((command.to_string(), arguments.clone()));
            let body = match command {
                "threads" => Ok(json!({ "threads": [{ "id": 7, "name": "main" }] })),
                "stackTrace" => Ok(json!({
                    "stackFrames": [{
                        "id": 99,
                        "name": "main",
                        "line": 3,
                        "column": 1,
                        "source": { "path": "/tmp/app.py" }
                    }],
                    "totalFrames": 1
                })),
                "scopes" => Ok(json!({
                    "scopes": [{ "name": "Locals", "variablesReference": 42, "expensive": false }]
                })),
                "variables" => Ok(json!({
                    "variables": [{ "name": "answer", "value": "42", "variablesReference": 0 }]
                })),
                "setBreakpoints" => {
                    let bps = arguments
                        .get("breakpoints")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|bp| {
                            json!({
                                "verified": true,
                                "line": bp.get("line").and_then(Value::as_u64).unwrap_or(1)
                            })
                        })
                        .collect::<Vec<_>>();
                    Ok(json!({ "breakpoints": bps }))
                }
                "continue" | "next" | "stepIn" | "stepOut" | "pause" => Ok(json!({})),
                "disconnect" => Ok(json!({})),
                other => Ok(json!({ "command": other })),
            }?;
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(json!({
                "type": "response",
                "request_seq": 1,
                "success": true,
                "command": command,
                "body": body
            }));
            Ok(PendingDapRequest {
                command: command.to_string(),
                rx,
            })
        }

        async fn wait_event(&self, names: &[&str], _wait: Duration) -> Result<Value, ToolError> {
            let mut events = self.events.lock().await;
            if let Some(index) = events.iter().position(|event| {
                event
                    .get("event")
                    .and_then(Value::as_str)
                    .is_some_and(|name| names.contains(&name))
            }) {
                Ok(events.remove(index))
            } else {
                Err(ToolError::Timeout { seconds: 1 })
            }
        }

        async fn shutdown(&self) {}
    }

    fn test_session(namespace: &str, transport: Arc<dyn DapTransport>) -> Arc<DapSession> {
        Arc::new(DapSession {
            id: "dap-test".to_string(),
            namespace: namespace.to_string(),
            adapter: "debugpy",
            mode: "launch",
            initialize: json!({ "supportsConfigurationDoneRequest": true }),
            transport,
            breakpoints: Arc::new(AsyncMutex::new(HashMap::new())),
        })
    }

    #[test]
    fn parses_content_length_frame_header() {
        let frame = b"Content-Length: 5\r\n\r\nhello";
        assert_eq!(parse_header(frame), Some((21, 5)));
    }

    #[test]
    fn breakpoint_lines_are_sorted_and_deduped() {
        let lines = breakpoint_lines(&json!({ "lines": [8, 2, 8, 3] })).unwrap();
        assert_eq!(lines, vec![2, 3, 8]);
    }

    #[tokio::test]
    async fn fake_session_sets_lists_and_clears_breakpoints() {
        let tmp = tempdir().expect("tempdir");
        let file = tmp.path().join("app.py");
        std::fs::write(&file, "x = 1\nprint(x)\n").expect("write");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let store = AsyncMutex::new(HashMap::new());
        let session = test_session("workspace", Arc::new(FakeDapTransport::new(Vec::new())));
        store
            .lock()
            .await
            .insert(store_key("workspace", "dap-test"), session);

        let set = execute_dap_tool(
            json!({
                "operation": "set_breakpoints",
                "session_id": "dap-test",
                "path": "app.py",
                "lines": [2]
            }),
            &ctx,
            &store,
        )
        .await
        .expect("set breakpoints");
        assert!(set.content.contains("\"verified\": true"));

        let listed = execute_dap_tool(
            json!({ "operation": "list_breakpoints", "session_id": "dap-test" }),
            &ctx,
            &store,
        )
        .await
        .expect("list breakpoints");
        let listed_json: Value = serde_json::from_str(&listed.content).expect("json result");
        assert_eq!(
            listed_json["breakpoints"][0]["lines"],
            json!([2]),
            "listed breakpoints should preserve verified line"
        );

        let cleared = execute_dap_tool(
            json!({ "operation": "clear_breakpoints", "session_id": "dap-test", "path": "app.py" }),
            &ctx,
            &store,
        )
        .await
        .expect("clear breakpoints");
        assert!(cleared.content.contains("\"cleared\""));
    }

    #[tokio::test]
    async fn fake_session_reads_stack_scopes_and_variables() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let store = AsyncMutex::new(HashMap::new());
        let session = test_session("workspace", Arc::new(FakeDapTransport::new(Vec::new())));
        store
            .lock()
            .await
            .insert(store_key("workspace", "dap-test"), session);

        let stack = execute_dap_tool(
            json!({ "operation": "stack", "session_id": "dap-test" }),
            &ctx,
            &store,
        )
        .await
        .expect("stack");
        assert!(stack.content.contains("\"stackFrames\""));
        assert!(stack.content.contains("\"main\""));

        let scopes = execute_dap_tool(
            json!({ "operation": "scopes", "session_id": "dap-test", "frame_id": 99 }),
            &ctx,
            &store,
        )
        .await
        .expect("scopes");
        assert!(scopes.content.contains("\"Locals\""));

        let variables = execute_dap_tool(
            json!({
                "operation": "variables",
                "session_id": "dap-test",
                "variables_reference": 42
            }),
            &ctx,
            &store,
        )
        .await
        .expect("variables");
        assert!(variables.content.contains("\"answer\""));
        assert!(variables.content.contains("\"42\""));
    }

    #[tokio::test]
    async fn fake_session_resume_returns_stopped_event() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let store = AsyncMutex::new(HashMap::new());
        let session = test_session(
            "workspace",
            Arc::new(FakeDapTransport::new(vec![json!({
                "type": "event",
                "event": "stopped",
                "body": { "threadId": 7, "reason": "breakpoint" }
            })])),
        );
        store
            .lock()
            .await
            .insert(store_key("workspace", "dap-test"), session);

        let resumed = execute_dap_tool(
            json!({ "operation": "continue", "session_id": "dap-test" }),
            &ctx,
            &store,
        )
        .await
        .expect("continue");
        assert!(resumed.content.contains("\"operation\": \"continue\""));
        assert!(resumed.content.contains("\"reason\": \"breakpoint\""));
    }

    #[tokio::test]
    async fn attach_rejects_non_localhost_targets_before_spawning() {
        let tmp = tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let store = AsyncMutex::new(HashMap::new());
        let err = execute_dap_tool(
            json!({
                "operation": "attach",
                "adapter": "debugpy",
                "host": "example.com",
                "port": 5678
            }),
            &ctx,
            &store,
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("localhost"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn debugpy_launch_fixture_reaches_breakpoint_when_available() {
        let debugpy_available = std::process::Command::new("python3")
            .args(["-c", "import debugpy"])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !debugpy_available {
            return;
        }

        let tmp = tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("app.py"),
            "value = 41\nvalue = value + 1\nprint(value)\n",
        )
        .expect("write fixture");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let store = AsyncMutex::new(HashMap::new());

        let started = execute_dap_tool(
            json!({
                "operation": "start",
                "adapter": "debugpy",
                "program": "app.py",
                "lines": [2],
                "wait_ms": 5000
            }),
            &ctx,
            &store,
        )
        .await
        .expect("start debugpy fixture");
        let started_json: Value = serde_json::from_str(&started.content).expect("start json");
        let session_id = started_json["session_id"]
            .as_str()
            .expect("session id")
            .to_string();
        assert_eq!(started_json["event"]["event"], json!("stopped"));

        let stack = execute_dap_tool(
            json!({ "operation": "stack", "session_id": session_id, "levels": 5 }),
            &ctx,
            &store,
        )
        .await
        .expect("stack");
        let stack_json: Value = serde_json::from_str(&stack.content).expect("stack json");
        let frame_id = stack_json["stackFrames"][0]["id"]
            .as_u64()
            .expect("frame id");
        assert_eq!(stack_json["stackFrames"][0]["name"], json!("<module>"));

        let scopes = execute_dap_tool(
            json!({ "operation": "scopes", "session_id": session_id, "frame_id": frame_id }),
            &ctx,
            &store,
        )
        .await
        .expect("scopes");
        let scopes_json: Value = serde_json::from_str(&scopes.content).expect("scopes json");
        let locals_ref = scopes_json["scopes"]
            .as_array()
            .and_then(|scopes| {
                scopes.iter().find_map(|scope| {
                    (scope["name"].as_str() == Some("Locals"))
                        .then(|| scope["variablesReference"].as_u64())
                        .flatten()
                })
            })
            .expect("locals variables reference");

        let variables = execute_dap_tool(
            json!({
                "operation": "variables",
                "session_id": session_id,
                "variables_reference": locals_ref
            }),
            &ctx,
            &store,
        )
        .await
        .expect("variables");
        let variables_json: Value = serde_json::from_str(&variables.content).expect("vars json");
        let has_value = variables_json["variables"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .any(|var| var["name"].as_str() == Some("value"));
        assert!(
            has_value,
            "expected local variable `value`: {variables_json}"
        );

        let _ = execute_dap_tool(
            json!({ "operation": "stop", "session_id": session_id }),
            &ctx,
            &store,
        )
        .await
        .expect("stop");
    }
}
