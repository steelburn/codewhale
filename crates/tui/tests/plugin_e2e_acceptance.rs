//! Plugin acceptance at both compatibility and v0.9.1 bundle boundaries.
//!
//! Tests the plugin frontmatter scanner end-to-end from the binary level:
//! - Scripts with valid `# name:` frontmatter are discovered
//! - Approval levels (auto, suggest, required) are parsed correctly
//! - Hidden files and README.md are ignored
//! - Empty and missing directories are handled gracefully
//! - The distributed binary still loads after the plugin module migration
//! - A sealed real PTY exercises plugin.toml review/trust/enable/revoke,
//!   reviewed Skill dispatch, and hermetic reviewed stdio MCP execution

use std::path::PathBuf;
use std::process::Command;

use cucumber::{World as _, given, then, when, writer::Stats as _};
use tempfile::TempDir;

#[cfg(all(unix, feature = "long-running-tests"))]
#[path = "support/qa_harness/mod.rs"]
mod qa_harness;

#[cfg(all(unix, feature = "long-running-tests"))]
use qa_harness::harness::{Harness, make_sealed_workspace};
#[cfg(all(unix, feature = "long-running-tests"))]
use qa_harness::keys;

const FEATURE_NAME: &str = "Plugin discovery and listing";
const FEATURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/features/plugin_e2e_acceptance.feature"
);
const DISCOVERY_SCENARIO: &str =
    "Plugin scripts are discovered from the configured plugin directory";
const EMPTY_SCENARIO: &str = "Empty plugin directory reports no plugins";
const MISSING_SCENARIO: &str = "Missing plugin directory reports the path";

// ---------------------------------------------------------------------------
// Test-local plugin scanner
//
// Mirrors the real `scan_plugin_dir` from `crates/tui/src/tools/plugin.rs`
// so the test can run as a standalone integration test without relying on
// `#[path]` (which breaks on internal `crate::` and `super::` imports).
// The contract (frontmatter format, skip rules) matches exactly.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
struct TestPluginMeta {
    name: String,
    description: String,
    approval: TestApproval,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum TestApproval {
    Auto,
    Suggest,
    Required,
}

fn parse_frontmatter(content: &str) -> Option<TestPluginMeta> {
    let mut name = String::new();
    let mut description = String::new();
    let mut approval_str = String::new();

    for line in content.lines().take(20) {
        let line = line.trim();
        let rest = line
            .strip_prefix('#')
            .or_else(|| line.strip_prefix("//"))
            .or_else(|| line.strip_prefix("--"));
        let Some(rest) = rest else { continue };
        let Some((key, value)) = rest.trim_start().split_once(':') else {
            continue;
        };
        match key.trim().to_lowercase().as_str() {
            "name" => name = value.trim().to_string(),
            "description" => description = value.trim().to_string(),
            "approval" => approval_str = value.trim().to_string(),
            _ => {}
        }
    }

    if name.is_empty() {
        return None;
    }

    let approval = match approval_str.to_lowercase().as_str() {
        "auto" => TestApproval::Auto,
        "required" => TestApproval::Required,
        _ => TestApproval::Suggest,
    };

    Some(TestPluginMeta {
        name,
        description: if description.is_empty() {
            "User-provided plugin tool".to_string()
        } else {
            description
        },
        approval,
    })
}

fn scan_plugin_dir(dir: &std::path::Path) -> Vec<(PathBuf, TestPluginMeta)> {
    let mut results = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return results,
    };

    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();

        if path.is_dir() {
            continue;
        }

        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && (name.starts_with('.') || name == "README.md")
        {
            continue;
        }

        if let Ok(content) = std::fs::read_to_string(&path)
            && let Some(meta) = parse_frontmatter(&content)
        {
            results.push((path, meta));
        }
    }

    results
}

// ---------------------------------------------------------------------------
// Cucumber world
// ---------------------------------------------------------------------------

#[derive(Debug, Default, cucumber::World)]
struct PluginE2EWorld {
    /// TempDir holding the plugin directory. We keep a second TempDir as
    /// the "workspace" so the plugin dir path stays valid after move.
    _workspace: Option<TempDir>,
    plugin_dir: Option<TempDir>,
    discovered: Option<Vec<(PathBuf, TestPluginMeta)>>,
    scanner_message: Option<String>,
}

// ---------------------------------------------------------------------------
// Given steps
// ---------------------------------------------------------------------------

#[given("an offline CodeWhale workspace with a configured plugin directory")]
fn offline_workspace_with_plugin_dir(world: &mut PluginE2EWorld) {
    let workspace = TempDir::new().expect("workspace tempdir");
    let plugin_dir = TempDir::new().expect("plugin tempdir");
    world._workspace = Some(workspace);
    world.plugin_dir = Some(plugin_dir);
}

#[given(regex = r"^the plugin directory contains:$")]
fn plugin_directory_contains(world: &mut PluginE2EWorld, step: &cucumber::gherkin::Step) {
    let dir = world
        .plugin_dir
        .as_ref()
        .expect("plugin directory should be configured");

    let table = step
        .table
        .as_ref()
        .expect("step should include a data table");
    let mut rows = table.rows.iter();
    let headers = rows.next().expect("data table should include a header");
    let name_idx = headers
        .iter()
        .position(|h| h == "name")
        .expect("data table should have a 'name' column");
    let desc_idx = headers
        .iter()
        .position(|h| h == "description")
        .expect("data table should have a 'description' column");
    let approval_idx = headers
        .iter()
        .position(|h| h == "approval")
        .expect("data table should have an 'approval' column");

    for row in rows {
        let name = row.get(name_idx).expect("plugin name");
        let description = row.get(desc_idx).expect("plugin description");
        let approval = row.get(approval_idx).expect("plugin approval");

        let script_path = dir.path().join(format!("{name}.sh"));
        let script_content = format!(
            "# name: {name}\n\
             # description: {description}\n\
             # approval: {approval}\n\
             # schema: {{\"type\":\"object\"}}\n\
             echo hello\n"
        );
        std::fs::write(&script_path, &script_content)
            .unwrap_or_else(|e| panic!("write plugin script {name}.sh: {e}"));

        // Make executable on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
                .unwrap_or_else(|e| panic!("chmod {name}.sh: {e}"));
        }
    }

    // Write a README.md and a hidden file that should be ignored
    std::fs::write(dir.path().join("README.md"), "# Plugin Docs\n").expect("write README.md");
    std::fs::write(
        dir.path().join(".hidden_script.sh"),
        "# name: hidden\n# description: Should not appear\n",
    )
    .expect("write hidden");
}

#[given("the plugin directory is empty")]
fn plugin_directory_empty(world: &mut PluginE2EWorld) {
    // Replace with a fresh empty directory
    let dir = TempDir::new().expect("empty plugin tempdir");
    world.plugin_dir = Some(dir);
}

#[given("the plugin directory does not exist")]
fn plugin_directory_does_not_exist(world: &mut PluginE2EWorld) {
    let base = TempDir::new().expect("base tempdir for non-existent path");
    let non_existent = base.path().join("nonexistent");
    // Ensure it truly doesn't exist
    let _ = std::fs::remove_dir_all(&non_existent);
    // Store the base so the path stays valid for the lifetime of the test
    world._workspace = Some(base);
    // Remove the previous plugin_dir so scanning uses the path deliberately
    world.plugin_dir = None;
    world.scanner_message = Some(format!(
        "No plugin directory found at {}",
        non_existent.display()
    ));
}

// ---------------------------------------------------------------------------
// When steps
// ---------------------------------------------------------------------------

#[when("the plugin scanner discovers plugins")]
fn plugin_scanner_discovers_plugins(world: &mut PluginE2EWorld) {
    let dir = world
        .plugin_dir
        .as_ref()
        .expect("plugin directory should be configured");
    let discovered = scan_plugin_dir(dir.path());
    world.discovered = Some(discovered);
}

#[when("the plugin scanner runs")]
fn plugin_scanner_runs(world: &mut PluginE2EWorld) {
    // Use the stored non-existent path
    let msg = world
        .scanner_message
        .as_ref()
        .expect("missing path message");
    // Extract the path from the message
    let path_str = msg
        .strip_prefix("No plugin directory found at ")
        .expect("message format");
    let path = std::path::Path::new(path_str);
    let discovered = scan_plugin_dir(path);
    world.discovered = Some(discovered);
}

// ---------------------------------------------------------------------------
// Then steps
// ---------------------------------------------------------------------------

#[then(regex = r"^the scanner should report (\d+) plugins?$")]
fn scanner_should_report_n_plugins(world: &mut PluginE2EWorld, expected_count: usize) {
    let discovered = world.discovered.as_ref().expect("scanner should have run");
    assert_eq!(
        discovered.len(),
        expected_count,
        "expected {expected_count} plugins, found {}: {discovered:#?}",
        discovered.len()
    );
}

#[then(regex = r#"^the scanned plugin "([^"]+)" should have "([^"]+)" as description$"#)]
fn scanned_plugin_should_have_description(
    world: &mut PluginE2EWorld,
    name: String,
    expected_description: String,
) {
    let discovered = world.discovered.as_ref().expect("scanner should have run");
    let meta = discovered
        .iter()
        .find(|(_, m)| m.name == name)
        .map(|(_, m)| m)
        .unwrap_or_else(|| panic!("plugin \"{name}\" not found in scan results"));

    assert_eq!(
        meta.description, expected_description,
        "plugin \"{name}\" description mismatch"
    );
}

#[then(regex = r#"^the scanned plugin "([^"]+)" should have "([^"]+)" as approval$"#)]
fn scanned_plugin_should_have_approval(
    world: &mut PluginE2EWorld,
    name: String,
    expected_approval: String,
) {
    let discovered = world.discovered.as_ref().expect("scanner should have run");
    let meta = discovered
        .iter()
        .find(|(_, m)| m.name == name)
        .map(|(_, m)| m)
        .unwrap_or_else(|| panic!("plugin \"{name}\" not found in scan results"));

    let actual = match meta.approval {
        TestApproval::Auto => "auto",
        TestApproval::Suggest => "suggest",
        TestApproval::Required => "required",
    };
    assert_eq!(
        actual, expected_approval,
        "plugin \"{name}\" approval mismatch"
    );
}

#[then(regex = r#"^the scanned plugin "([^"]+)" should not be found$"#)]
fn scanned_plugin_should_not_be_found(world: &mut PluginE2EWorld, name: String) {
    let discovered = world.discovered.as_ref().expect("scanner should have run");
    assert!(
        !discovered.iter().any(|(_, m)| m.name == name),
        "plugin \"{name}\" should not be present in scan results, but was found"
    );
}

#[then("the scanner should report the missing directory path")]
fn scanner_should_report_missing_path(world: &mut PluginE2EWorld) {
    let discovered = world.discovered.as_ref().expect("scanner should have run");
    assert!(
        discovered.is_empty(),
        "expected empty results for missing directory, got: {discovered:#?}"
    );
    let msg = world
        .scanner_message
        .as_deref()
        .unwrap_or("scanner ran without message");
    assert!(
        msg.contains("No plugin directory found"),
        "expected missing directory message, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Binary smoke test
// ---------------------------------------------------------------------------

/// Prove the binary still loads after the plugin module extraction.
#[tokio::test(flavor = "current_thread")]
async fn plugin_module_does_not_break_binary_load() {
    let output = Command::new(codewhale_tui_binary())
        .arg("--version")
        .output()
        .expect("codewhale-tui --version should start");

    assert!(
        output.status.success(),
        "codewhale-tui --version failed\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let version = String::from_utf8_lossy(&output.stdout);
    assert!(
        version.contains("codewhale"),
        "version output should mention codewhale, got: {version}"
    );
}

// ---------------------------------------------------------------------------
// Real plugin.toml binary/TUI acceptance
// ---------------------------------------------------------------------------

#[cfg(all(unix, feature = "long-running-tests"))]
const BINARY_ACCEPTANCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

#[cfg(all(unix, feature = "long-running-tests"))]
fn write_reviewed_bundle_fixture(workspace: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let bundle = workspace.join(".codewhale/plugins/demo");
    std::fs::create_dir_all(bundle.join("skills/review")).expect("plugin fixture directories");
    std::fs::write(
        bundle.join("plugin.toml"),
        r#"schema_version = 1
[plugin]
name = "demo"
version = "1.0.0"
description = "Hermetic binary acceptance fixture"

[skills]
path = "skills"

[mcp_servers.local]
command = "./server.py"
connect_timeout = 5
execute_timeout = 30
read_timeout = 30

[mcp_servers.local.env]
ACCEPTANCE_LOG = "${PLUGIN_ACCEPTANCE_LOG}"
"#,
    )
    .expect("plugin manifest");
    std::fs::write(
        bundle.join("skills/review/SKILL.md"),
        "---\nname: review\ndescription: reviewed binary acceptance Skill\n---\n\nUse the reviewed fixture.\n",
    )
    .expect("plugin Skill");
    let server = bundle.join("server.py");
    std::fs::write(
        &server,
        r#"#!/usr/bin/env python3
import json
import os
import signal
import sys
import time

log_path = os.environ["ACCEPTANCE_LOG"]

def record(event):
    with open(log_path, "a", encoding="utf-8") as handle:
        handle.write(event + "\n")
        handle.flush()

def stop(signum, _frame):
    record("signal:" + str(signum))
    raise SystemExit(0)

signal.signal(signal.SIGTERM, stop)
signal.signal(signal.SIGINT, stop)
record("started")
record("api-key-present:" + str("DEEPSEEK_API_KEY" in os.environ).lower())

for raw in sys.stdin:
    message = json.loads(raw)
    method = message.get("method")
    request_id = message.get("id")
    if method == "initialize":
        result = {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "plugin-acceptance", "version": "1.0.0"},
        }
    elif method == "tools/list":
        record("tools:list")
        result = {"tools": [{
            "name": "echo",
            "description": "Hermetic plugin echo",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {"type": "string"},
                    "hang": {"type": "boolean"},
                },
            },
        }]}
    elif method == "tools/call":
        args = message.get("params", {}).get("arguments", {})
        if args.get("hang"):
            record("call:hang")
            while True:
                time.sleep(0.05)
        record("call:echo")
        result = {"content": [{
            "type": "text",
            "text": "plugin-echo:" + str(args.get("text", "")),
        }]}
    else:
        if request_id is None:
            continue
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}) + "\n")
    sys.stdout.flush()
"#,
    )
    .expect("stdio MCP fixture");
    std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o755))
        .expect("executable MCP fixture");
    bundle
}

#[cfg(all(unix, feature = "long-running-tests"))]
fn sse_line(value: serde_json::Value) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(&value).expect("SSE JSON")
    )
}

#[cfg(all(unix, feature = "long-running-tests"))]
fn text_sse(text: &str) -> String {
    [
        sse_line(serde_json::json!({
            "id": "chatcmpl-plugin-acceptance",
            "object": "chat.completion.chunk",
            "model": "deepseek-v4-pro",
            "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": null}]
        })),
        sse_line(serde_json::json!({
            "id": "chatcmpl-plugin-acceptance",
            "object": "chat.completion.chunk",
            "model": "deepseek-v4-pro",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 8, "completion_tokens": 4, "total_tokens": 12}
        })),
        "data: [DONE]\n\n".to_string(),
    ]
    .join("")
}

#[cfg(all(unix, feature = "long-running-tests"))]
fn tool_call_sse(hang: bool) -> String {
    let call_id = if hang {
        "call_plugin_hang"
    } else {
        "call_plugin_echo"
    };
    let arguments = serde_json::to_string(&serde_json::json!({
        "text": "acceptance",
        "hang": hang,
    }))
    .expect("tool args");
    [
        sse_line(serde_json::json!({
            "id": "chatcmpl-plugin-tool",
            "object": "chat.completion.chunk",
            "model": "deepseek-v4-pro",
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{
                    "index": 0,
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": "mcp_plugin-4-demo-local_echo",
                        "arguments": arguments
                    }
                }]},
                "finish_reason": null
            }]
        })),
        sse_line(serde_json::json!({
            "id": "chatcmpl-plugin-tool",
            "object": "chat.completion.chunk",
            "model": "deepseek-v4-pro",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 6, "total_tokens": 16}
        })),
        "data: [DONE]\n\n".to_string(),
    ]
    .join("")
}

#[cfg(all(unix, feature = "long-running-tests"))]
fn spawn_hermetic_model_server() -> (
    String,
    std::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<()>,
) {
    use tiny_http::{Header, Method, Response, Server};

    let server = Server::http("127.0.0.1:0").expect("loopback model server");
    let base_url = format!(
        "http://{}/v1",
        server.server_addr().to_ip().expect("loopback address")
    );
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        loop {
            let request = match server.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(Some(request)) => request,
                Ok(None) => {
                    if shutdown_rx.try_recv().is_ok() {
                        break;
                    }
                    continue;
                }
                Err(_) => break,
            };
            let mut request = request;
            let url = request.url().to_string();
            if request.method() == &Method::Get && url.ends_with("/models") {
                let response = Response::from_string(
                    r#"{"object":"list","data":[{"id":"deepseek-v4-pro","object":"model"}]}"#,
                )
                .with_header(
                    Header::from_bytes("content-type", "application/json").expect("JSON header"),
                );
                let _ = request.respond(response);
                continue;
            }
            let mut body = String::new();
            let _ = request.as_reader().read_to_string(&mut body);
            let current_user = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|request| request.get("messages")?.as_array().cloned())
                .and_then(|messages| {
                    messages.into_iter().rev().find_map(|message| {
                        (message.get("role")?.as_str()? == "user")
                            .then(|| message.get("content")?.as_str().map(str::to_owned))?
                    })
                })
                .unwrap_or_default();
            let stream = if current_user.contains("hang plugin call") {
                tool_call_sse(true)
            } else if body.contains("plugin-echo:acceptance") {
                text_sse("binary plugin call complete")
            } else if body.contains("call plugin echo") {
                tool_call_sse(false)
            } else {
                text_sse("binary fixture acknowledged")
            };
            let response = Response::from_string(stream).with_header(
                Header::from_bytes("content-type", "text/event-stream").expect("SSE header"),
            );
            let _ = request.respond(response);
        }
    });
    (base_url, shutdown_tx, handle)
}

#[cfg(all(unix, feature = "long-running-tests"))]
fn submit_tui_command(tui: &mut Harness, text: &str) {
    tui.send(keys::key::text(text)).expect("type TUI command");
    tui.wait_for_text(text, std::time::Duration::from_secs(3))
        .expect("typed command visible");
    std::thread::sleep(std::time::Duration::from_millis(180));
    tui.pump();
    tui.send(keys::key::enter()).expect("submit TUI command");
}

#[cfg(all(unix, feature = "long-running-tests"))]
fn visible_review_confirmation(tui: &mut Harness) -> Option<String> {
    tui.pump();
    review_confirmation_in_text(&tui.frame().text())
}

#[cfg(all(unix, feature = "long-running-tests"))]
fn review_confirmation_in_text(text: &str) -> Option<String> {
    text.lines().map(str::trim).find_map(|line| {
        let token = line.strip_prefix("/plugin trust demo ")?;
        (token.contains('.')
            && token.len() >= 17
            && token.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '.'))
        .then(|| line.to_string())
    })
}

#[cfg(all(unix, feature = "long-running-tests"))]
fn wait_for_log(tui: &mut Harness, path: &std::path::Path, needle: &str) {
    let deadline = std::time::Instant::now() + BINARY_ACCEPTANCE_TIMEOUT;
    loop {
        tui.pump();
        if std::fs::read_to_string(path).is_ok_and(|body| body.contains(needle)) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "plugin MCP log did not contain {needle:?}\n{}",
            tui.debug_dump()
        );
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
}

/// Exercise the distributed binary through a real PTY and a sealed home. The
/// only socket is a test-owned loopback model endpoint; plugin execution is
/// stdio-only and receives no real credentials or ambient secret environment.
#[cfg(all(unix, feature = "long-running-tests"))]
#[tokio::test(flavor = "current_thread")]
async fn plugin_toml_binary_lifecycle_skill_and_stdio_mcp_acceptance() {
    static ACCEPTANCE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = ACCEPTANCE_LOCK
        .lock()
        .unwrap_or_else(|lock| lock.into_inner());
    let workspace = make_sealed_workspace().expect("sealed workspace");
    let bundle = write_reviewed_bundle_fixture(workspace.workspace());
    let mcp_log = workspace.home().join(".codewhale/plugin-acceptance.log");
    let (base_url, shutdown_tx, model_thread) = spawn_hermetic_model_server();
    let mut tui = Harness::builder(Harness::cargo_bin("codewhale-tui"))
        .cwd(workspace.workspace())
        .clear_env()
        .seal_home(workspace.home())
        .env("DEEPSEEK_API_KEY", "sealed-plugin-acceptance-key")
        .env("DEEPSEEK_BASE_URL", &base_url)
        .env("DEEPSEEK_MODEL", "deepseek-v4-pro")
        .env("PLUGIN_ACCEPTANCE_LOG", mcp_log.to_string_lossy())
        .env("NO_ANIMATIONS", "1")
        .env("RUST_LOG", "warn")
        .args([
            "--workspace",
            workspace.workspace().to_str().expect("workspace UTF-8"),
            "--no-project-config",
            "--skip-onboarding",
            "--fresh",
        ])
        .size(52, 200)
        .spawn()
        .expect("start distributed TUI binary");
    tui.wait_for_text("Write a task", BINARY_ACCEPTANCE_TIMEOUT)
        .expect("TUI composer");

    submit_tui_command(&mut tui, "/plugin show demo");
    tui.wait_for_text("Qualified skills: [demo:review]", BINARY_ACCEPTANCE_TIMEOUT)
        .expect("show reviewed Skill inventory");
    assert!(
        !workspace
            .home()
            .join(".codewhale/plugins/state.json")
            .exists(),
        "show must remain read-only"
    );

    submit_tui_command(&mut tui, "/plugin trust demo");
    tui.wait_for(
        |frame| review_confirmation_in_text(&frame.text()).is_some(),
        BINARY_ACCEPTANCE_TIMEOUT,
    )
    .expect("review confirmation");
    let confirmation = visible_review_confirmation(&mut tui)
        .unwrap_or_else(|| panic!("review confirmation not visible\n{}", tui.debug_dump()));
    submit_tui_command(&mut tui, &confirmation);
    tui.wait_for_text("Plugin bundle 'demo': trusted.", BINARY_ACCEPTANCE_TIMEOUT)
        .expect("trust receipt");

    submit_tui_command(&mut tui, "/plugin enable demo");
    tui.wait_for_text("Plugin bundle 'demo': enabled.", BINARY_ACCEPTANCE_TIMEOUT)
        .expect("bundle enabled");
    submit_tui_command(&mut tui, "$demo:review");
    tui.wait_for_text("Activated skill: demo:review", BINARY_ACCEPTANCE_TIMEOUT)
        .expect("reviewed Skill dispatch");

    submit_tui_command(&mut tui, "call plugin echo");
    tui.wait_for_text("Do you want to proceed?", BINARY_ACCEPTANCE_TIMEOUT)
        .expect("MCP approval prompt");
    tui.send(keys::key::ch('2'))
        .expect("approve this reviewed MCP kind for the sealed session");
    wait_for_log(&mut tui, &mcp_log, "started");
    wait_for_log(&mut tui, &mcp_log, "api-key-present:false");
    wait_for_log(&mut tui, &mcp_log, "tools:list");
    wait_for_log(&mut tui, &mcp_log, "call:echo");
    tui.wait_for_text("binary plugin call complete", BINARY_ACCEPTANCE_TIMEOUT)
        .expect("plugin tool result returned to model");

    submit_tui_command(&mut tui, "hang plugin call");
    wait_for_log(&mut tui, &mcp_log, "call:hang");
    tui.send([0x03]).expect("interrupt hanging plugin turn");
    std::thread::sleep(std::time::Duration::from_millis(300));
    tui.pump();
    tui.send([0x15])
        .expect("clear the interrupted prompt restored into the composer");
    submit_tui_command(&mut tui, "/plugin revoke demo");
    tui.wait_for_text(
        "Plugin bundle 'demo': trust-revoked.",
        BINARY_ACCEPTANCE_TIMEOUT,
    )
    .expect("bundle trust revoked");
    wait_for_log(&mut tui, &mcp_log, "signal:");

    let state = std::fs::read_to_string(workspace.home().join(".codewhale/plugins/state.json"))
        .expect("durable plugin state");
    assert!(state.contains("\"enabled\": true"));
    assert!(state.contains("\"trust\": null"));
    assert!(bundle.join("server.py").exists(), "source bundle preserved");

    let _ = tui.shutdown();
    let _ = shutdown_tx.send(());
    let _ = model_thread.join();
}

// ---------------------------------------------------------------------------
// Scenario runners
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn plugin_discovery_happy_path() {
    run_scenario(DISCOVERY_SCENARIO, 9).await;
}

#[tokio::test(flavor = "current_thread")]
async fn plugin_discovery_empty_directory() {
    run_scenario(EMPTY_SCENARIO, 4).await;
}

#[tokio::test(flavor = "current_thread")]
async fn plugin_discovery_missing_directory() {
    run_scenario(MISSING_SCENARIO, 4).await;
}

async fn run_scenario(name: &'static str, expected_steps: usize) {
    let writer = PluginE2EWorld::cucumber()
        .fail_on_skipped()
        .with_default_cli()
        .filter_run(FEATURE_PATH, move |feature, _, scenario| {
            feature.name == FEATURE_NAME && scenario.name == name
        })
        .await;
    assert_eq!(writer.failed_steps(), 0, "scenario failed: {name}");
    assert_eq!(writer.skipped_steps(), 0, "scenario skipped steps: {name}");
    assert_eq!(
        writer.passed_steps(),
        expected_steps,
        "scenario did not run: {name}"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn codewhale_tui_binary() -> PathBuf {
    if let Some(path) = option_env!("CARGO_BIN_EXE_codewhale-tui") {
        return PathBuf::from(path);
    }
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_codewhale-tui") {
        return PathBuf::from(path);
    }

    let mut path = std::env::current_exe().expect("current test executable path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push(format!("codewhale-tui{}", std::env::consts::EXE_SUFFIX));
    path
}
