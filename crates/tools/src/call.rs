use codewhale_protocol::{ToolKind, ToolPayload};
use serde::{Deserialize, Serialize};

/// Identifies where a tool call originated from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallSource {
    /// Direct invocation from the model or user.
    Direct,
    /// Invocation through the JavaScript REPL environment.
    JsRepl,
}

/// A tool invocation request before it has been validated and dispatched.
///
/// Contains the tool name, its input payload, and metadata about where the
/// call originated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Name of the tool to invoke.
    pub name: String,
    /// The input payload for the tool.
    pub payload: ToolPayload,
    /// Where this call originated (direct or REPL).
    pub source: ToolCallSource,
    /// Optional raw tool-call identifier from the upstream provider.
    pub raw_tool_call_id: Option<String>,
}

impl ToolCall {
    /// Derive the execution subject for this call.
    ///
    /// For local shell payloads this returns the shell command and its
    /// working directory; for all other payloads the tool name and the
    /// provided `fallback_cwd` are returned instead. The third element
    /// of the tuple is a human-readable kind label (`"shell"` or `"tool"`).
    pub fn execution_subject(&self, fallback_cwd: &str) -> (String, String, &'static str) {
        match &self.payload {
            ToolPayload::LocalShell { params } => (
                params.command.clone(),
                params
                    .cwd
                    .clone()
                    .unwrap_or_else(|| fallback_cwd.to_string()),
                "shell",
            ),
            _ => (self.name.clone(), fallback_cwd.to_string(), "tool"),
        }
    }
}

/// A validated tool invocation ready to be handled.
///
/// Created by the registry after a [`ToolCall`] passes validation, this
/// carries all the context a [`ToolHandler`] needs to execute the tool.
#[derive(Debug, Clone)]
pub struct ToolInvocation {
    /// Unique identifier for this invocation (generated or from the provider).
    pub call_id: String,
    /// Name of the tool being invoked.
    pub tool_name: String,
    /// The input payload for the tool.
    pub payload: ToolPayload,
    /// Where this invocation originated.
    pub source: ToolCallSource,
}

/// Errors that can occur during tool dispatch and execution.
///
/// Unlike [`ToolError`], which represents input validation failures within
/// a tool, `FunctionCallError` covers problems at the dispatch layer: the
/// tool was not found, its kind did not match, it was rejected because it
/// is mutating, it timed out, was cancelled, or its handler returned an
/// error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FunctionCallError {
    /// No tool with the given name is registered.
    ToolNotFound { name: String },
    /// The payload kind does not match the handler's expected kind.
    KindMismatch { expected: ToolKind, got: ToolKind },
    /// The tool is mutating but `allow_mutating` was `false`.
    MutatingToolRejected { name: String },
    /// The tool execution exceeded its configured timeout.
    TimedOut { name: String, timeout_ms: u64 },
    /// The tool execution was cancelled.
    Cancelled { name: String },
    /// The tool handler returned an error.
    ExecutionFailed { name: String, error: String },
}
