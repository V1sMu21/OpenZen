import { writable, derived, get } from "svelte/store";
import { isTauri, tauriInvoke } from "../api/tauri";
import en from "./en.json";
import zh from "./zh.json";

const translations: Record<string, Record<string, string>> = { en, zh };

export const locale = writable<string>("zh");

export const t = derived(locale, ($locale) => {
  return (key: string, fallback?: string): string => {
    return translations[$locale]?.[key] ?? fallback ?? key;
  };
});

export function tSync(localeVal: string, key: string, fallback?: string): string {
  return translations[localeVal]?.[key] ?? fallback ?? key;
}

export function localT(key: string, fallback?: string): string {
  return translations[get(locale)]?.[key] ?? fallback ?? key;
}

function applyDocumentLang(lang: string): void {
  if (typeof document !== "undefined") {
    document.documentElement.lang = lang;
  }
}

export async function initLocale(): Promise<void> {
  let lang = "zh";
  if (isTauri()) {
    try {
      lang = await tauriInvoke("get_locale");
    } catch {
      lang = "zh";
    }
  } else {
    const saved = localStorage.getItem("openzen-locale");
    lang = saved === "en" ? "en" : "zh";
  }
  locale.set(lang);
  applyDocumentLang(lang);
}

export async function switchLocale(lang: "zh" | "en"): Promise<void> {
  locale.set(lang);
  applyDocumentLang(lang);
  if (isTauri()) {
    await tauriInvoke("set_locale", { lang });
  } else {
    localStorage.setItem("openzen-locale", lang);
  }
}
