import { describe, expect, it } from "vitest";
import { splitDeliveryNote } from "./deliveryNote";

/**
 * The delivery block is system metadata appended to the final reply. The
 * bubble must show the answer alone; the block moves behind the footer
 * toggle. These fixtures are the exact shapes found in
 * ~/.openzen/openzen/sessions.json (2026-11-01 skill-sync run), including
 * the legacy `完成：` value that spans unquoted lines.
 */
describe("交付说明块剥离", () => {
  it("剥离 ⚠️ 行 + 交付说明，保留完整正文", () => {
    const content = [
      "## 做了什么",
      "",
      "装好了。",
      "",
      "> ⚠️ 独立评审未通过（high 问题 ×4），修复预算已耗尽",
      "",
      "---",
      "> **📋 交付说明**",
      "> 完成：## 做了什么",
      "",
      "**盘点**（ZCode / DSH → OpenZen）",
      "",
      "| 来源 | 结果 |",
      "> 验证：无 task_spec.md，未运行验收断言（未验证）；独立评审：证据不足，未通过（未验证）",
      "> 遗留：独立评审未通过（high 问题 ×4），修复预算已耗尽",
    ].join("\n");

    const { body, note } = splitDeliveryNote(content);
    expect(body).toBe("## 做了什么\n\n装好了。");
    expect(note).not.toBeNull();
    expect(note?.warning).toBe("独立评审未通过（high 问题 ×4），修复预算已耗尽");
    expect(note?.done).toContain("**盘点**（ZCode / DSH → OpenZen）");
    expect(note?.done).toContain("| 来源 | 结果 |");
    expect(note?.verified).toContain("无 task_spec.md");
    expect(note?.left).toBe("独立评审未通过（high 问题 ×4），修复预算已耗尽");
  });

  it("新版块没有 完成：/遗留： 时也能解析", () => {
    const content =
      "结果如下。\n\n---\n> **📋 交付说明**\n> 验证：断言 2/2 通过\n";
    const { body, note } = splitDeliveryNote(content);
    expect(body).toBe("结果如下。");
    expect(note?.verified).toBe("断言 2/2 通过");
    expect(note?.done).toBeUndefined();
    expect(note?.left).toBeUndefined();
  });

  it("英文交付说明同样剥离", () => {
    const content =
      "All done.\n\n---\n> **📋 Delivery notes**\n> Verified: 2/2 assertions passed\n> Left open: 1 pending todo\n";
    const { body, note } = splitDeliveryNote(content);
    expect(body).toBe("All done.");
    expect(note?.verified).toBe("2/2 assertions passed");
    expect(note?.left).toBe("1 pending todo");
  });

  it("普通回复末尾的引用块不动", () => {
    const content = "正文\n\n> 这是一句正常的引用，不是交付说明。";
    const { body, note } = splitDeliveryNote(content);
    expect(body).toBe(content);
    expect(note).toBeNull();
  });

  it("只有 ⚠️ 行、没有交付说明时不猜", () => {
    const content = "正文\n\n> ⚠️ 这一行可能是模型自己写的";
    const { body, note } = splitDeliveryNote(content);
    expect(body).toBe(content);
    expect(note).toBeNull();
  });

  it("代码块里引用的交付说明不动", () => {
    const content = [
      "示例：",
      "```",
      "---",
      "> **📋 交付说明**",
      "> 验证：这是示例",
      "```",
    ].join("\n");
    const { body, note } = splitDeliveryNote(content);
    expect(body).toBe(content);
    expect(note).toBeNull();
  });

  it("CRLF 也能识别", () => {
    const content =
      "正文\r\n\r\n---\r\n> **📋 交付说明**\r\n> 验证：通过\r\n";
    const { body, note } = splitDeliveryNote(content);
    expect(body).toBe("正文");
    expect(note?.verified).toBe("通过");
  });

  it("正文只保留到元数据块之前，尾部空白被裁掉", () => {
    const content = "答案\n\n\n---\n> **📋 交付说明**\n> 验证：—\n";
    const { body } = splitDeliveryNote(content);
    expect(body).toBe("答案");
  });
});
