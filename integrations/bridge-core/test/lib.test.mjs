import test from "node:test";
import assert from "node:assert/strict";

import {
  activeTurnBlock,
  commandAction,
  envFirst,
  parseBool,
  parseCommand,
  parseEnvText,
  parseList,
  parseTextContent,
  preservedChatStateFields,
  splitMessage,
  stripGroupPrefix
} from "../src/lib.mjs";

test("env and primitive parsers handle bridge env conventions", () => {
  assert.equal(envFirst({ A: "", B: " value " }, "A", "B"), "value");
  assert.deepEqual(parseList(" a, b ,, "), ["a", "b"]);
  assert.equal(parseBool("yes"), true);
  assert.equal(parseBool("0", true), false);
  assert.deepEqual(parseEnvText("export A='one'\nB=\"two\"\n# nope"), { A: "one", B: "two" });
});

test("parseTextContent supports plain text and JSON text/content wrappers", () => {
  assert.equal(parseTextContent("hello"), "hello");
  assert.equal(parseTextContent(JSON.stringify({ text: "hello" })), "hello");
  assert.equal(parseTextContent(JSON.stringify({ content: "hello" })), "hello");
});

test("stripGroupPrefix supports direct chat types and prefixed group text", () => {
  assert.deepEqual(
    stripGroupPrefix("inspect", {
      chatType: "private",
      requirePrefix: true,
      prefix: "/cw",
      directChatTypes: ["private"]
    }),
    { accepted: true, text: "inspect" }
  );
  assert.deepEqual(
    stripGroupPrefix("/cw inspect", {
      chatType: "group",
      requirePrefix: true,
      prefix: "/cw",
      directChatTypes: ["private"]
    }),
    { accepted: true, text: "inspect" }
  );
});

test("commands map common actions while menu/start stay opt in", () => {
  assert.deepEqual(parseCommand("/allow@CodeWhaleBot ap_1 remember", { stripBotMention: true }), {
    name: "allow",
    args: "ap_1 remember"
  });
  assert.deepEqual(parseCommand("/allow@CodeWhaleBot ap_1 remember"), {
    name: "allow@codewhalebot",
    args: "ap_1 remember"
  });
  assert.deepEqual(commandAction(parseCommand("/status")), { kind: "status" });
  assert.deepEqual(commandAction(parseCommand("/menu")), { kind: "prompt", prompt: "/menu" });
  assert.deepEqual(commandAction(parseCommand("/menu"), { allowMenu: true }), { kind: "menu" });
  assert.deepEqual(commandAction(parseCommand("/start"), { allowStart: true }), { kind: "help" });
});

test("state/message/runtime helpers preserve bridge behavior", () => {
  assert.deepEqual(
    preservedChatStateFields({ model: "m", replyToMessageId: "r", ignored: true }, [
      "model",
      "replyToMessageId"
    ]),
    { model: "m", replyToMessageId: "r" }
  );
  assert.deepEqual(splitMessage("a🧪b", 2), ["a🧪", "b"]);
  assert.deepEqual(activeTurnBlock({ turns: [{ id: "t1", status: "queued" }] }), {
    turnId: "t1",
    message: "Thread already has active turn t1. Wait for it to finish or send /interrupt."
  });
});
