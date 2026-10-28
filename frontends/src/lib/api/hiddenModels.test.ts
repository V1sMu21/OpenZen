import { beforeEach, describe, expect, it } from "vitest";
import {
  filterHidden,
  hiddenModelNames,
  hideModel,
  restoreHiddenModels,
  unhideModel,
} from "./hiddenModels";

const store = new Map<string, string>();
(globalThis as unknown as { localStorage: Storage }).localStorage = {
  getItem: (k: string) => store.get(k) ?? null,
  setItem: (k: string, v: string) => void store.set(k, v),
  removeItem: (k: string) => void store.delete(k),
  clear: () => store.clear(),
  key: () => null,
  length: 0,
} as Storage;

describe("hidden models", () => {
  beforeEach(() => {
    store.clear();
  });

  it("filters a hidden model out of every list", () => {
    hideModel("Agents_A1_8bit");
    const models = [{ name: "Agents_A1_8bit" }, { name: "GLM-5.3-FLASH" }];
    expect(filterHidden(models).map((m) => m.name)).toEqual(["GLM-5.3-FLASH"]);
  });

  it("restores everything and can unhide a single name", () => {
    hideModel("a");
    hideModel("b");
    unhideModel("a");
    expect(hiddenModelNames()).toEqual(["b"]);
    restoreHiddenModels();
    expect(hiddenModelNames()).toEqual([]);
  });

  it("survives a corrupt persisted value", () => {
    store.set("openzen.hiddenModels.v1", "{not json");
    expect(hiddenModelNames()).toEqual([]);
  });
});
