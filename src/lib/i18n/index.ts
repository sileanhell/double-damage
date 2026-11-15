import { storage } from "../storage";
import { createEffect, createRoot, createSignal } from "solid-js";
import { en } from "./en";
import { ru } from "./ru";

export type MessageKey = keyof typeof ru;
export type Messages = Record<MessageKey, string>;

export const LOCALES = ["en", "ru"] as const;
export type Locale = (typeof LOCALES)[number];
/** What the user picked in settings: a concrete locale or "system" to follow the OS. */
export type LocaleSetting = Locale | "system";

/** Native names, shown as-is regardless of the current language. */
export const LOCALE_NAMES: Record<Locale, string> = { en: "English", ru: "Русский" };

const FALLBACK: Locale = "en";
const DICTS: Record<Locale, Messages> = { en, ru };
const KEY = "dd:locale";

/**
 * First supported language from the OS/browser preference list. In Tauri the webview fills
 * `navigator.languages` from the OS (WebView2 on Windows, WKWebView on macOS, LANG on Linux).
 */
function detectLocale(): Locale {
  const preferred = navigator.languages?.length ? navigator.languages : [navigator.language];
  for (const tag of preferred) {
    const base = tag?.toLowerCase().split("-")[0];
    if ((LOCALES as readonly string[]).includes(base)) return base as Locale;
  }
  return FALLBACK;
}

function readSetting(): LocaleSetting {
  const stored = storage.getItem(KEY);
  return stored === "system" || (LOCALES as readonly string[]).includes(stored ?? "") ? (stored as LocaleSetting) : "system";
}

const [localeSetting, setLocaleSettingSignal] = createSignal<LocaleSetting>(readSetting());
export { localeSetting };

const [systemLocale, setSystemLocale] = createSignal<Locale>(detectLocale());
/** The OS/browser language, as far as we support it. */
export { systemLocale };
window.addEventListener("languagechange", () => setSystemLocale(detectLocale()));

/** The locale actually in use. */
export const locale = (): Locale => {
  const setting = localeSetting();
  return setting === "system" ? systemLocale() : setting;
};

export function setLocaleSetting(next: LocaleSetting) {
  setLocaleSettingSignal(next);
  if (next === "system") storage.removeItem(KEY);
  else storage.setItem(KEY, next);
}

/** Translates `key` for the current locale (reactive), filling `{name}` placeholders from `params`. */
export function t(key: MessageKey, params?: Record<string, string | number>): string {
  const text = DICTS[locale()][key] ?? DICTS[FALLBACK][key] ?? key;
  return params ? text.replace(/\{(\w+)\}/g, (match, name) => (name in params ? String(params[name]) : match)) : text;
}

createRoot(() => {
  createEffect(() => {
    document.documentElement.lang = locale();
  });
});
