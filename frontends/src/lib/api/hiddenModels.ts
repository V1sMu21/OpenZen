// User-hidden models: entries removed from the /model picker (e.g. a local
// deployment deleted on the omlx side). Persisted per install; does NOT edit
// mykey.toml, so it is reversible via restore.
//
// Every model list must go through `filterHidden` — the picker alone used to
// filter, so Settings still showed names the user had just deleted.

export const HIDDEN_MODELS_KEY = "openzen.hiddenModels.v1";
export const HIDDEN_MODELS_CHANGED = "openzen:hidden-models-changed";

export function hiddenModelNames(): string[] {
  try {
    const raw = localStorage.getItem(HIDDEN_MODELS_KEY);
    const list = raw ? JSON.parse(raw) : [];
    return Array.isArray(list) ? list.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

function saveHiddenModels(names: string[]) {
  try {
    localStorage.setItem(HIDDEN_MODELS_KEY, JSON.stringify([...new Set(names)]));
  } catch {
    /* private mode */
  }
  if (typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent(HIDDEN_MODELS_CHANGED));
  }
}

export function hideModel(name: string) {
  saveHiddenModels([...hiddenModelNames(), name]);
}

export function unhideModel(name: string) {
  saveHiddenModels(hiddenModelNames().filter((n) => n !== name));
}

export function restoreHiddenModels() {
  try {
    localStorage.removeItem(HIDDEN_MODELS_KEY);
  } catch {
    /* ignore */
  }
  if (typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent(HIDDEN_MODELS_CHANGED));
  }
}

export function filterHidden<T extends { name: string }>(list: T[]): T[] {
  const hidden = new Set(hiddenModelNames());
  return list.filter((m) => !hidden.has(m.name));
}
