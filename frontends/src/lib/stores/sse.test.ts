/**
 * SSE 事件归并（round5 P1-13④）。
 *
 * 守的是什么：`protocol_v1_batch` 是 2026-10 为长任务加的 IPC 批量信封
 * （后端 16ms / 64 条合并一次）。前端必须把单发与批量**归并成同一串事件
 * 序列**，且顺序不变 —— 顺序错了卡片顺序就错，形状错了所有流式事件静默
 * 消失（review finding B-6 的 Tauri/HTTP 双路径分叉就是这么来的）。
 */
import { describe, expect, it } from "vitest";
import { protocolEventsFrom, protocolEventFrom } from "./sse";

const ev = (type: string, extra: Record<string, unknown> = {}) => ({ type, ...extra });

describe("protocolEventsFrom — 单发与批量归并", () => {
  it("单发 protocol_v1：payload 本身就是事件", () => {
    const payload = ev("text_delta", { id: "t0", text: "hi" });
    const got = protocolEventsFrom(JSON.stringify(payload));
    expect(got).toEqual([payload]);
  });

  it("批量 protocol_v1_batch：展开成有序事件数组", () => {
    const events = [
      ev("text_delta", { id: "t0", text: "a" }),
      ev("tool_input_start", { tool_call_id: "c1", name: "read" }),
      ev("tool_input_available", { tool_call_id: "c1", args: '{"file_path":"x"}' }),
      ev("tool_output_available", { tool_call_id: "c1", output: "ok" }),
      ev("text_delta", { id: "t0", text: "b" }),
    ];
    const got = protocolEventsFrom(JSON.stringify({ type: "protocol_v1_batch", events }));
    expect(got.map((e) => e.type)).toEqual([
      "text_delta",
      "tool_input_start",
      "tool_input_available",
      "tool_output_available",
      "text_delta",
    ]);
    // 顺序必须逐条保持（不是集合语义）。用 toMatchObject 而不是
    // `got[0].text`：ProtocolV1Event 是可辨识联合，直接取字段过不了
    // svelte-check 的类型门。
    expect(got[0]).toMatchObject({ type: "text_delta", text: "a" });
    expect(got[4]).toMatchObject({ type: "text_delta", text: "b" });
  });

  it("批量与逐条单发得到同一串事件（归并等价性）", () => {
    const events = [
      ev("reasoning_start", { id: "r0" }),
      ev("reasoning_delta", { id: "r0", text: "think" }),
      ev("reasoning_end", { id: "r0" }),
    ];
    const batched = protocolEventsFrom(
      JSON.stringify({ type: "protocol_v1_batch", events }),
    );
    const oneByOne = events.flatMap((e) => protocolEventsFrom(JSON.stringify(e)));
    expect(batched).toEqual(oneByOne);
  });

  it("Tauri 路径的信封：data 字段是 JSON 字符串，要先剥一层", () => {
    const payload = ev("text_delta", { id: "t0", text: "wrapped" });
    const envelope = { session_id: "s1", event_type: "protocol_v1", data: JSON.stringify(payload) };
    expect(protocolEventsFrom(envelope)).toEqual([payload]);
    expect(protocolEventFrom(envelope)).toEqual(payload);
  });

  it("data 已经是对象（非字符串）时同样可用", () => {
    const payload = ev("text_delta", { id: "t0", text: "obj" });
    expect(protocolEventsFrom({ data: payload })).toEqual([payload]);
  });
});

describe("protocolEventsFrom — 坏输入只丢弃、不抛", () => {
  it("非法 JSON 字符串 → 空数组", () => {
    expect(protocolEventsFrom("{not json")).toEqual([]);
  });

  it("null / undefined / 数字 / 布尔 → 空数组", () => {
    for (const bad of [null, undefined, 42, true]) {
      expect(protocolEventsFrom(bad)).toEqual([]);
    }
  });

  it("events 不是数组时退化为把整体当单个事件（不误丢）", () => {
    const payload = ev("text_delta", { id: "t0", text: "x", events: "nope" });
    expect(protocolEventsFrom(JSON.stringify(payload))).toEqual([payload]);
  });

  it("批量里混入非对象条目 → 过滤掉，其余保留", () => {
    const events = [ev("text_delta", { text: "keep" }), null, "junk", 7];
    const got = protocolEventsFrom(JSON.stringify({ type: "protocol_v1_batch", events }));
    expect(got).toHaveLength(1);
    expect(got[0]).toMatchObject({ type: "text_delta", text: "keep" });
  });

  it("空批量 → 空数组（不是 [{}]）", () => {
    expect(protocolEventsFrom(JSON.stringify({ type: "protocol_v1_batch", events: [] }))).toEqual(
      [],
    );
  });
});

describe("分层契约：解析层不做会话过滤", () => {
  it("带外来 session_id 的 payload 仍被完整解析出来", () => {
    // 会话守卫（`session_id !== currentId` ⇒ 走后台路径、不进
    // handleSSEEvent）在 dispatcher 里，不在解析层。把它下沉到解析层会让
    // 后台会话的卡片永远不更新（applyBackgroundProtocolEvent 也拿不到
    // 事件）。这条断言钉住分层，防止有人"顺手"把过滤写进 protocolEventsFrom。
    const payload = {
      type: "text_delta",
      id: "t0",
      text: "from another session",
      session_id: "someone-else",
    };
    expect(protocolEventsFrom(JSON.stringify(payload))).toEqual([payload]);
  });
});
