import { describe, expect, it } from "vitest";
import { isNewerVersion, parseVersion } from "./version";

describe("release version comparison", () => {
  it("parses v-prefixed and plain tags", () => {
    expect(parseVersion("v0.3.0")).toEqual([0, 3, 0]);
    expect(parseVersion("0.2.6")).toEqual([0, 2, 6]);
    expect(parseVersion("v1.2")).toEqual([1, 2, 0]);
  });

  it("detects the 0.2.6 -> 0.3.0 update", () => {
    expect(isNewerVersion("v0.3.0", "0.2.6")).toBe(true);
    expect(isNewerVersion("0.2.6", "0.2.6")).toBe(false);
    expect(isNewerVersion("v0.2.5", "0.2.6")).toBe(false);
  });

  it("compares numeric segments, not strings", () => {
    expect(isNewerVersion("0.10.0", "0.9.9")).toBe(true);
    expect(isNewerVersion("1.0.0", "0.99.99")).toBe(true);
  });
});
