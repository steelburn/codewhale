import { readFile, writeFile, mkdir, rename, chmod } from "node:fs/promises";
import path from "node:path";

import {
  activeTurnBlock,
  cleanEnvValue,
  commandAction as coreCommandAction,
  compactRuntimeError,
  isPlaceholderValue,
  latestRunningTurn,
  parseApprovalDecisionArgs,
  parseBool,
  parseCommand,
  parseList,
  parseTextContent as coreParseTextContent,
  preservedChatStateFields,
  splitMessage,
  stripGroupPrefix as coreStripGroupPrefix
} from "../../bridge-core/src/lib.mjs";

export {
  activeTurnBlock,
  cleanEnvValue,
  compactRuntimeError,
  isPlaceholderValue,
  latestRunningTurn,
  parseApprovalDecisionArgs,
  parseBool,
  parseCommand,
  parseList,
  preservedChatStateFields,
  splitMessage
};

export function requiredEnv(name) {
  const value = process.env[name];
  if (!value || !value.trim()) {
    throw new Error(`${name} is required`);
  }
  return value.trim();
}

export function parseTextContent(content) {
  return coreParseTextContent(content, ["text"]);
}

export function incomingIdentity(body) {
  const from = body?.from || {};
  const chatId = body.chatid || (body.chattype === "single" && from.userid ? `single:${from.userid}` : "");
  return {
    chatId,
    messageId: body.msgid || "",
    chatType: body.chattype || "single",
    userId: from.userid || "",
    aibotId: body.aibotid || ""
  };
}

export function isAllowed(identity, allowlist, allowUnlisted = false) {
  if (allowUnlisted) return true;
  const allowed = new Set(allowlist);
  return [identity.chatId, identity.userId].filter(Boolean).some((id) => allowed.has(id));
}

export function pairingRefusalText(identity) {
  return [
    "This chat is not in WECOM_CHAT_ALLOWLIST.",
    `chat_id=${identity.chatId}`,
    identity.userId ? `user_id=${identity.userId}` : ""
  ]
    .filter(Boolean)
    .join("\n");
}

export function stripGroupPrefix(text, { chatType, requirePrefix, prefix }) {
  return coreStripGroupPrefix(text, {
    chatType,
    requirePrefix,
    prefix: prefix || "/ds",
    directChatTypes: ["single"]
  });
}

export function commandAction(command) {
  return coreCommandAction(command);
}

export function helpText() {
  return [
    "CodeWhale 企业微信桥接命令:",
    "/help - 显示帮助",
    "/status - runtime 和工作区状态",
    "/threads - 最近的 runtime 线程",
    "/new - 为此聊天创建新线程",
    "/resume <thread_id> - 绑定到此聊天的现有线程",
    "/model <name|default> - 设置或重置聊天模型",
    "/interrupt - 中断活动 turn",
    "/compact - 压缩当前线程",
    "/allow <approval_id> [remember] - 批准待处理的工具调用",
    "/deny <approval_id> - 拒绝待处理的工具调用",
    "",
    "其他所有内容均作为 CodeWhale 提示发送。"
  ].join("\n");
}

export class ThreadStore {
  static async open(filePath) {
    const store = new ThreadStore(filePath);
    await store.load();
    return store;
  }

  constructor(filePath) {
    this.filePath = filePath;
    this.data = { chats: {} };
  }

  async load() {
    try {
      const raw = await readFile(this.filePath, "utf8");
      this.data = JSON.parse(raw);
      if (!this.data.chats) this.data.chats = {};
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }

  async getChat(chatId) {
    return this.data.chats[chatId] || null;
  }

  listChats() {
    return Object.entries(this.data.chats || {});
  }

  async setChat(chatId, state) {
    this.data.chats[chatId] = state;
    await this.save();
    return state;
  }

  async patchChat(chatId, patch) {
    const current = this.data.chats[chatId] || {};
    this.data.chats[chatId] = { ...current, ...patch };
    await this.save();
    return this.data.chats[chatId];
  }

  async save() {
    const dir = path.dirname(this.filePath);
    await mkdir(dir, { recursive: true, mode: 0o700 });
    await chmodBestEffort(dir, 0o700);
    const tmp = `${this.filePath}.tmp`;
    await writeFile(tmp, `${JSON.stringify(this.data, null, 2)}\n`, { mode: 0o600 });
    await chmodBestEffort(tmp, 0o600);
    await rename(tmp, this.filePath);
    await chmodBestEffort(this.filePath, 0o600);
  }
}

async function chmodBestEffort(filePath, mode) {
  try {
    await chmod(filePath, mode);
  } catch (error) {
    if (process.platform !== "win32") throw error;
  }
}

export function validateBridgeConfig(env) {
  const errors = [];
  const warnings = [];
  const info = [];
  const add = (list, code, message) => list.push({ code, message });

  for (const key of ["WECOM_BOT_ID", "WECOM_BOT_SECRET"]) {
    const value = cleanEnvValue(env[key]);
    if (!value) {
      add(errors, "missing_required", `${key} is required`);
    } else if (isPlaceholderValue(value)) {
      add(errors, "placeholder_value", `${key} still contains a placeholder value`);
    }
  }

  const runtimeUrl = cleanEnvValue(env.CODEWHALE_RUNTIME_URL || "http://127.0.0.1:7878");
  try {
    const parsed = new URL(runtimeUrl);
    if (!["http:", "https:"].includes(parsed.protocol)) {
      add(errors, "invalid_runtime_url", "CODEWHALE_RUNTIME_URL must use http or https");
    }
  } catch {
    add(errors, "invalid_runtime_url", "CODEWHALE_RUNTIME_URL is not a valid URL");
  }

  const runtimeToken = cleanEnvValue(env.CODEWHALE_RUNTIME_TOKEN);
  if (!runtimeToken) {
    add(errors, "missing_runtime_token", "CODEWHALE_RUNTIME_TOKEN is required");
  } else if (isPlaceholderValue(runtimeToken)) {
    add(errors, "placeholder_runtime_token", "CODEWHALE_RUNTIME_TOKEN is still a placeholder");
  }

  const allowUnlisted = parseBool(env.WECOM_ALLOW_UNLISTED, false);
  const allowlist = parseList(env.WECOM_CHAT_ALLOWLIST);

  if (!allowlist.length && allowUnlisted) {
    add(warnings, "pairing_mode_open", "WECOM_ALLOW_UNLISTED=true leaves first-pairing mode open");
  } else if (!allowlist.length) {
    add(warnings, "not_paired", "WECOM_CHAT_ALLOWLIST is empty; all chats will be refused");
  }

  return { ok: errors.length === 0, errors, warnings, info };
}

export function formatValidationReport(result) {
  const lines = ["WeCom bridge config validation"];
  for (const item of result.errors) lines.push(`[fail] ${item.message}`);
  for (const item of result.warnings) lines.push(`[warn] ${item.message}`);
  for (const item of result.info) lines.push(`[info] ${item.message}`);
  if (result.ok) lines.push("[ok] No blocking config errors found");
  return lines.join("\n");
}
