export function envFirst(env, ...names) {
  for (const name of names) {
    const value = env?.[name];
    if (value != null && String(value).trim()) return String(value).trim();
  }
  return "";
}

export function parseList(raw) {
  return String(raw || "")
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

export function parseBool(raw, fallback = false) {
  if (raw == null || raw === "") return fallback;
  return ["1", "true", "yes", "on"].includes(String(raw).trim().toLowerCase());
}

export function parseEnvText(raw) {
  const env = {};
  for (const line of String(raw || "").split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;
    const normalized = trimmed.startsWith("export ") ? trimmed.slice(7).trim() : trimmed;
    const index = normalized.indexOf("=");
    if (index <= 0) continue;
    const key = normalized.slice(0, index).trim();
    let value = normalized.slice(index + 1).trim();
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1);
    }
    env[key] = value;
  }
  return env;
}

export function cleanEnvValue(value) {
  return String(value ?? "").trim();
}

export function isPlaceholderValue(value) {
  const normalized = cleanEnvValue(value).toLowerCase();
  return (
    !normalized ||
    normalized.includes("replace-with") ||
    normalized.includes("xxxxxxxx") ||
    normalized === "changeme"
  );
}

export function parseTextContent(content, keys = ["text", "content"]) {
  if (typeof content !== "string") return "";
  try {
    const parsed = JSON.parse(content);
    for (const key of keys) {
      if (typeof parsed?.[key] === "string") return parsed[key];
    }
  } catch {
    return content;
  }
  return content;
}

export function stripGroupPrefix(text, { chatType, requirePrefix, prefix, directChatTypes = [] }) {
  const trimmed = String(text || "").trim();
  if (!trimmed) return { accepted: false, text: "" };
  if (!requirePrefix || directChatTypes.includes(chatType)) {
    return { accepted: true, text: trimmed };
  }
  const marker = prefix || "/ds";
  if (trimmed === marker) return { accepted: true, text: "/help" };
  if (trimmed.startsWith(`${marker} `)) {
    return { accepted: true, text: trimmed.slice(marker.length).trim() };
  }
  return { accepted: false, text: "" };
}

export function parseCommand(text, options = {}) {
  const trimmed = String(text || "").trim();
  if (!trimmed.startsWith("/")) return { name: "prompt", args: trimmed };
  const [head, ...rest] = trimmed.split(/\s+/);
  const rawName = head.slice(1);
  const name = (options.stripBotMention ? rawName.split("@")[0] : rawName).toLowerCase();
  return {
    name,
    args: rest.join(" ").trim()
  };
}

export function parseApprovalDecisionArgs(args) {
  const parts = String(args || "")
    .split(/\s+/)
    .filter(Boolean);
  return {
    approvalId: parts[0] || "",
    remember: parts.slice(1).includes("remember")
  };
}

export function commandAction(command, options = {}) {
  const allowMenu = options.allowMenu === true;
  const allowStart = options.allowStart === true;
  switch (command.name) {
    case "start":
      if (allowStart) return { kind: "help" };
      break;
    case "help":
      return { kind: "help" };
    case "menu":
      if (allowMenu) return { kind: "menu" };
      break;
    case "status":
      return { kind: "status" };
    case "threads":
      return { kind: "threads" };
    case "new":
      return { kind: "new_thread" };
    case "resume":
      return { kind: "resume", threadId: command.args };
    case "interrupt":
      return { kind: "interrupt" };
    case "compact":
      return { kind: "compact" };
    case "model":
      return { kind: "set_model", modelName: command.args };
    case "allow":
      return { kind: "approval", decision: "allow", ...parseApprovalDecisionArgs(command.args) };
    case "deny":
      return { kind: "approval", decision: "deny", ...parseApprovalDecisionArgs(command.args) };
    case "prompt":
      return { kind: "prompt", prompt: command.args };
    default:
      break;
  }
  return {
    kind: "prompt",
    prompt: `/${command.name}${command.args ? ` ${command.args}` : ""}`
  };
}

export function preservedChatStateFields(state = {}, fields = ["model"]) {
  const preserved = {};
  for (const field of fields) {
    if (Object.prototype.hasOwnProperty.call(state || {}, field)) {
      preserved[field] = state[field] || null;
    }
  }
  return preserved;
}

export function splitMessage(text, maxChars = 3500) {
  const value = String(text || "");
  const chars = Array.from(value);
  if (chars.length <= maxChars) return value ? [value] : [];
  const chunks = [];
  let cursor = 0;
  while (cursor < chars.length) {
    chunks.push(chars.slice(cursor, cursor + maxChars).join(""));
    cursor += maxChars;
  }
  return chunks;
}

export function compactRuntimeError(status, body) {
  const message =
    body?.error?.message ||
    body?.message ||
    (typeof body === "string" ? body : JSON.stringify(body));
  return `Runtime API request failed (${status}): ${message}`;
}

export function latestRunningTurn(detail) {
  const turns = Array.isArray(detail?.turns) ? detail.turns : [];
  for (let index = turns.length - 1; index >= 0; index -= 1) {
    const turn = turns[index];
    if (["queued", "in_progress"].includes(turn?.status)) return turn;
  }
  return null;
}

export function activeTurnBlock(detail, state = {}) {
  const runningTurn = latestRunningTurn(detail);
  if (!runningTurn) return null;
  return {
    turnId: runningTurn.id || state.activeTurnId || "",
    message: `Thread already has active turn ${
      runningTurn.id || state.activeTurnId || "(unknown)"
    }. Wait for it to finish or send /interrupt.`
  };
}
