/**
 * `convertStreamEventsToParts`（round5 P1-13④）。
 *
 * 守的是什么：这是"重开 App / 切会话后历史气泡怎么重建"的唯一入口。
 * 2026-10 的落盘压缩（P0-1）曾经把 `reasoning_delta` 整类丢掉，注释还
 * 声称"思考正文在 message.thinking 里"—— 但全前端没有一处渲染它，于是
 * 195 条消息的历史思考块在重载后**整块消失**（review finding A/B/E-1，
 * 置信度 88/90/88）。本文件的第一条断言就是那条回归的看门狗。
 */
import { describe, expect, it } from "vitest";
import { convertStreamEventsToParts } from "./parts";

const ev = (type: string, extra: Record<string, unknown> = {}) => ({ type, ...extra });

describe("思考块重建（P0-1 回归看门狗）", () => {
  it("reasoning_start + 合并后的 delta + end → 非空思考文本", () => {
    // 这正是 compact_stream_events 现在落盘的形状：相邻同 id 的 delta
    // 已被合并成一条，但**没有被删除**。
    const parts = convertStreamEventsToParts([
      ev("reasoning_start", { id: "r0" }),
      ev("reasoning_delta", { id: "r0", text: "先看 checkpoint 的 turn 号，" }),
      ev("reasoning_delta", { id: "r0", text: "再决定解析哪个文件。" }),
      ev("reasoning_end", { id: "r0", duration_ms: 4200 }),
    ]);
    const reasoning = parts.filter((p) => p.type === "reasoning");
    expect(reasoning).toHaveLength(1);
    // 空 text 的 reasoning part 会被 ChatMessage 的 `!p.text?.trim()` 过滤掉
    // —— 也就是"思考块整块消失"。所以这条断言必须是非空的。
    expect((reasoning[0] as { text: string }).text).toBe(
      "先看 checkpoint 的 turn 号，再决定解析哪个文件。",
    );
    expect((reasoning[0] as { durationMs?: number }).durationMs).toBe(4200);
    expect((reasoning[0] as { state: string }).state).toBe("done");
  });

  it("只有 start/end 没有 delta 时确实是空的（说明 delta 是唯一正文来源）", () => {
    // 反向断言：这条测试存在的意义是证明"丢掉 delta ⇒ 丢掉正文"，
    // 即上面那条断言不是恒真的。
    const parts = convertStreamEventsToParts([
      ev("reasoning_start", { id: "r0" }),
      ev("reasoning_end", { id: "r0" }),
    ]);
    const reasoning = parts.filter((p) => p.type === "reasoning");
    expect(reasoning).toHaveLength(1);
    expect((reasoning[0] as { text: string }).text).toBe("");
  });

  it("多个思考块各自独立，不串味", () => {
    const parts = convertStreamEventsToParts([
      ev("reasoning_start", { id: "r0" }),
      ev("reasoning_delta", { id: "r0", text: "第一段" }),
      ev("reasoning_end", { id: "r0" }),
      ev("reasoning_start", { id: "r1" }),
      ev("reasoning_delta", { id: "r1", text: "第二段" }),
      ev("reasoning_end", { id: "r1" }),
    ]);
    const texts = parts
      .filter((p) => p.type === "reasoning")
      .map((p) => (p as { text: string }).text);
    expect(texts).toEqual(["第一段", "第二段"]);
  });
});

describe("文本与工具卡重建", () => {
  it("text_delta 累加成一条文本 part", () => {
    const parts = convertStreamEventsToParts([
      ev("text_start", { id: "t0" }),
      ev("text_delta", { id: "t0", text: "你好，" }),
      ev("text_delta", { id: "t0", text: "世界" }),
      ev("text_end", { id: "t0" }),
    ]);
    const text = parts.filter((p) => p.type === "text");
    expect(text).toHaveLength(1);
    expect((text[0] as { text: string }).text).toBe("你好，世界");
  });

  it("tool_input_available 带出工具名与参数，tool_output_available 带出结果", () => {
    const parts = convertStreamEventsToParts([
      ev("tool_input_start", { tool_call_id: "c1", name: "read" }),
      ev("tool_input_available", {
        tool_call_id: "c1",
        name: "read",
        args: '{"file_path":"/tmp/a.md"}',
      }),
      ev("tool_output_available", { tool_call_id: "c1", name: "read", output: "hello" }),
    ]);
    const tool = parts.find((p) => p.type === "tool-invocation") as
      | { name?: string; args?: string; result?: string; state?: string }
      | undefined;
    expect(tool, "tool-invocation part must be produced").toBeTruthy();
    expect(tool!.name).toBe("read");
    expect(tool!.args).toBe('{"file_path":"/tmp/a.md"}');
    expect(tool!.result).toBe("hello");
    expect(tool!.state).toBe("output-available");
  });

  it("空事件数组 → 空 parts（不产生幽灵卡片）", () => {
    expect(convertStreamEventsToParts([])).toEqual([]);
  });
});
