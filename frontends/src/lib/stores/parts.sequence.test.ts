/**
 * Chat parts reducer 序列不变量（S2 前端状态机）。
 *
 * 守的是什么：Svelte 5 的 `{#each}` 遇到重复 key 会整块放弃渲染
 * （2026-10-23 的 `each_key_duplicate` 就是这么白屏的）。工具卡片的 key
 * 来自 `toolCallId`，所以同一个 id 无论 start/delta/available 以什么顺序、
 * 重复几次到达，都只能产生一个 part，且参数不能丢。
 */
import { describe, expect, it } from "vitest";
import { convertStreamEventsToParts, mergeFinalReply } from "./parts";

type Ev = Record<string, unknown> & { type: string };

function lcg(seed: number) {
  let s = seed >>> 0;
  return () => {
    s = (Math.imul(s, 1664525) + 1013904223) >>> 0;
    return s;
  };
}

const ALPHABET = [
  (id: string) => ({ type: "text_start", id }),
  (id: string) => ({ type: "text_delta", id, text: `t-${id}` }),
  (id: string) => ({ type: "text_end", id }),
  (id: string) => ({ type: "reasoning_start", id }),
  (id: string) => ({ type: "reasoning_delta", id, text: `r-${id}` }),
  (id: string) => ({ type: "reasoning_end", id }),
  (id: string) => ({ type: "tool_input_start", tool_call_id: id, name: "write" }),
  (id: string) => ({ type: "tool_input_delta", tool_call_id: id, delta: "{\"a\":" }),
  (id: string) => ({
    type: "tool_input_available",
    tool_call_id: id,
    name: "write",
    args: "{\"a\":1}",
  }),
  (id: string) => ({ type: "tool_output_available", tool_call_id: id, name: "write", output: "ok" }),
];

describe("parts reducer 序列不变量", () => {
  it("300 条随机事件序列：不抛异常，工具 part 的 toolCallId 唯一", () => {
    let seed = 0x20261118;
    for (let n = 0; n < 300; n++) {
      const rnd = lcg(seed++);
      const len = 5 + (rnd() % 25);
      const events: Ev[] = [];
      for (let i = 0; i < len; i++) {
        const id = `id${rnd() % 3}`;
        const make = ALPHABET[rnd() % ALPHABET.length];
        events.push(make(id) as Ev);
      }
      const parts = convertStreamEventsToParts(events);
      const toolIds = parts
        .filter((p) => p.type === "tool-invocation")
        .map((p) => (p as { toolCallId: string }).toolCallId);
      expect(new Set(toolIds).size, `duplicate toolCallId in sequence ${n}: ${toolIds}`).toBe(
        toolIds.length,
      );
    }
  });

  it("同一个 tool_call_id 重复 start 时只产生一个工具 part", () => {
    const parts = convertStreamEventsToParts([
      { type: "tool_input_start", tool_call_id: "tc1", name: "write" },
      { type: "tool_input_start", tool_call_id: "tc1", name: "write" },
      { type: "tool_input_delta", tool_call_id: "tc1", delta: "{\"file_path\":\"a\"}" },
      { type: "tool_input_available", tool_call_id: "tc1", name: "write", args: "{\"file_path\":\"a\"}" },
      { type: "tool_output_available", tool_call_id: "tc1", name: "write", output: "written" },
    ]);
    const tools = parts.filter((p) => p.type === "tool-invocation") as Array<{
      toolCallId: string;
      args: string;
      result?: string;
    }>;
    expect(tools).toHaveLength(1);
    expect(tools[0].toolCallId).toBe("tc1");
    expect(tools[0].args).toContain("file_path");
    expect(tools[0].result).toBe("written");
  });

  it("delta 先于 start 到达时不崩溃，后续 start 仍补齐同一个 part", () => {
    const parts = convertStreamEventsToParts([
      { type: "tool_input_delta", tool_call_id: "tc2", delta: "{\"x\":" },
      { type: "tool_input_start", tool_call_id: "tc2", name: "write" },
      { type: "tool_input_delta", tool_call_id: "tc2", delta: "1}" },
      { type: "tool_input_available", tool_call_id: "tc2", name: "write", args: "{\"x\":1}" },
    ]);
    const tools = parts.filter((p) => p.type === "tool-invocation") as Array<{ toolCallId: string }>;
    expect(tools).toHaveLength(1);
    expect(tools[0].toolCallId).toBe("tc2");
  });

  it("同一 text id 的 start/delta/delta/end 合并成一个 part", () => {
    const parts = convertStreamEventsToParts([
      { type: "text_start", id: "t1" },
      { type: "text_delta", id: "t1", text: "ab" },
      { type: "text_delta", id: "t1", text: "cd" },
      { type: "text_end", id: "t1" },
    ]);
    const texts = parts.filter((p) => p.type === "text") as Array<{ text: string; state: string }>;
    expect(texts).toHaveLength(1);
    expect(texts[0].text).toBe("abcd");
    expect(texts[0].state).toBe("done");
  });
});

describe("final reply merge", () => {
  it("replaces the streamed copy even when a tool card sits after it", () => {
    const parts = [
      { type: "text", id: "t1", text: "hello", state: "done" },
      {
        type: "tool-invocation",
        toolCallId: "c1",
        name: "write",
        args: "{}",
        state: "done",
      },
    ] as any[];
    const preferred =
      "hello\n\n---\n> **📋 Delivery notes**\n> Verified: 1/1";
    const out = mergeFinalReply(parts, preferred);
    const texts = out.filter((p) => p.type === "text");
    expect(texts).toHaveLength(1);
    expect(texts[0].text).toContain("Delivery notes");
    expect(out[1].type).toBe("tool-invocation");
  });

  it("leaves an exact match alone", () => {
    const parts = [{ type: "text", id: "t1", text: "hi", state: "done" }] as any[];
    expect(mergeFinalReply(parts, "hi")).toBe(parts);
  });
});
