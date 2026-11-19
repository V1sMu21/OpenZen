import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

vi.mock("../api/tauri", () => ({
  isTauri: () => false,
  tauriInvoke: vi.fn(async () => undefined),
}));

import { approval } from "./approval";

const req = (id: string) => ({
  requestId: id,
  sessionId: "s1",
  toolName: "code_run",
  pattern: "git",
  arguments: {},
  approvedCount: 0,
  currentLevel: "workspace",
  origin: null,
}) as never;

describe("审批倒计时", () => {
  beforeEach(() => approval.reset());

  it("push 后 deadline 必须被写入，不能被旧快照覆盖为 0", () => {
    approval.push(req("r1"));
    const s = get(approval);
    expect(s.current?.requestId).toBe("r1");
    expect(s.deadline).toBeGreaterThan(Date.now());
    expect(s.countdown).toBeGreaterThan(1700);
  });

  it("重复投递同一个 requestId 不会重复入队", () => {
    approval.push(req("hold"));
    approval.push(req("r2"));
    approval.push(req("r2"));
    const s = get(approval);
    expect(s.current?.requestId).toBe("hold");
    expect(s.queue.filter((r) => r.requestId === "r2")).toHaveLength(1);
  });
});
