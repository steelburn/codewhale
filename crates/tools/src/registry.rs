use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use codewhale_protocol::ToolOutput;

use crate::call::{FunctionCallError, ToolCall, ToolInvocation};
use crate::handler::ToolHandler;
use crate::runtime::{TOOL_EXECUTION_LOCK_HELD, ToolCallRuntime, tool_payload_kind};
use crate::spec::ConfiguredToolSpec;
use crate::spec::ToolSpec;

/// Central registry that maps tool names to their specs and handlers.
///
/// Use [`register()`](ToolRegistry::register) to add tools, then
/// [`dispatch()`](ToolRegistry::dispatch) to invoke them. The registry
/// owns a [`ToolCallRuntime`] that manages concurrent execution.
#[derive(Default)]
pub struct ToolRegistry {
    handlers: HashMap<String, Arc<dyn ToolHandler>>,
    specs: HashMap<String, ConfiguredToolSpec>,
    runtime: ToolCallRuntime,
}

impl ToolRegistry {
    /// Register a tool with its specification and handler.
    ///
    /// The tool's name is taken from `spec.name`. Returns an error if
    /// registration fails (currently infallible, but the `Result` is
    /// reserved for future validation).
    pub fn register(&mut self, spec: ToolSpec, handler: Arc<dyn ToolHandler>) -> Result<()> {
        let name = spec.name.clone();
        self.specs.insert(
            name.clone(),
            ConfiguredToolSpec {
                supports_parallel_tool_calls: spec.supports_parallel_tool_calls,
                spec,
            },
        );
        self.handlers.insert(name, handler);
        Ok(())
    }

    /// Return the configured specs for every registered tool.
    pub fn list_specs(&self) -> Vec<ConfiguredToolSpec> {
        self.specs.values().cloned().collect()
    }

    /// Validate and execute a tool call.
    ///
    /// Looks up the tool by name, verifies the payload kind matches the
    /// handler, enforces the `allow_mutating` guard, acquires the
    /// appropriate execution lock, and forwards the call to the handler.
    /// Returns a [`FunctionCallError`] if any validation step fails or
    /// the handler returns an error.
    pub async fn dispatch(
        &self,
        call: ToolCall,
        allow_mutating: bool,
    ) -> std::result::Result<ToolOutput, FunctionCallError> {
        let handler = self.handlers.get(&call.name).cloned().ok_or_else(|| {
            FunctionCallError::ToolNotFound {
                name: call.name.clone(),
            }
        })?;
        let configured =
            self.specs
                .get(&call.name)
                .cloned()
                .ok_or_else(|| FunctionCallError::ToolNotFound {
                    name: call.name.clone(),
                })?;

        let payload_kind = tool_payload_kind(&call.payload);
        let expected = handler.kind();
        if !handler.matches_kind(payload_kind) {
            return Err(FunctionCallError::KindMismatch {
                expected,
                got: payload_kind,
            });
        }
        if handler.is_mutating() && !allow_mutating {
            return Err(FunctionCallError::MutatingToolRejected { name: call.name });
        }

        let invocation = ToolInvocation {
            call_id: call
                .raw_tool_call_id
                .clone()
                .unwrap_or_else(|| format!("tool-call-{}", uuid::Uuid::new_v4())),
            tool_name: call.name.clone(),
            payload: call.payload,
            source: call.source,
        };

        let _guard = self
            .runtime
            .acquire(configured.supports_parallel_tool_calls)
            .await;

        TOOL_EXECUTION_LOCK_HELD
            .scope(
                (),
                self.execute_with_timeout(handler, configured.spec.timeout_ms, invocation),
            )
            .await
    }

    async fn execute_with_timeout(
        &self,
        handler: Arc<dyn ToolHandler>,
        timeout_ms: Option<u64>,
        invocation: ToolInvocation,
    ) -> std::result::Result<ToolOutput, FunctionCallError> {
        if let Some(timeout_ms) = timeout_ms {
            let name = invocation.tool_name.clone();
            match tokio::time::timeout(
                Duration::from_millis(timeout_ms),
                handler.handle(invocation),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err(FunctionCallError::TimedOut { name, timeout_ms }),
            }
        } else {
            handler.handle(invocation).await
        }
    }
}
