import { isTauri } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";

/** Whether the URL may be handed to the system browser (the opener capability allows http(s) only). */
export const canOpen = (url: string) => /^https?:\/\//i.test(url);

/** Opens an http(s) URL in the system browser; never navigates the webview. */
export async function openExternal(url: string): Promise<void> {
  if (!canOpen(url)) return;
  if (isTauri()) await openUrl(url);
  else window.open(url, "_blank", "noopener,noreferrer");
}

/** The URL split for display, so the host (what actually decides where it goes) stands out. */
export function urlParts(href: string): { before: string; host: string; after: string } {
  try {
    const url = new URL(href);
    if (url.protocol === "mailto:") return { before: "mailto:", host: decodeURIComponent(url.pathname), after: url.search };
    const user = url.username ? `${url.username}${url.password ? ":…" : ""}@` : "";
    return { before: `${url.protocol}//${user}`, host: url.host, after: `${url.pathname}${url.search}${url.hash}` };
  } catch {
    return { before: "", host: href, after: "" };
  }
}
