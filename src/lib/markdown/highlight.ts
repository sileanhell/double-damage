import type { HLJSApi } from "highlight.js";
import { createRoot, createSignal } from "solid-js";

/**
 * highlight.js, loaded in its own chunk right after this module is imported (so it doesn't weigh
 * on the first render); code shows unhighlighted for the few ms until it arrives.
 *
 * Measured (highlight.js common, ~TS code): ~0.35 ms per KB with a known language, but
 * `highlightAuto` costs 10+ ms even for small blocks. So:
 * - known language: highlighted every frame while streaming, up to STREAM_LIMIT;
 * - no language: auto-detected only once the block is complete and small (AUTO_LIMIT),
 *   among a short list of likely languages.
 */
const STREAM_LIMIT = 8_000;
const AUTO_LIMIT = 3_000;
const AUTO_LANGUAGES = ["typescript", "javascript", "python", "bash", "json", "rust", "go", "java", "cpp", "csharp", "xml", "css", "sql", "yaml"];

const [api, setApi] = createRoot(() => createSignal<HLJSApi>());
void import("highlight.js/lib/common").then((module) => setApi(() => module.default));

const cache = new Map<string, string | null>();
const CACHE_SIZE = 200;

/**
 * Highlighted HTML for `code` (reactive: undefined until highlight.js has loaded), or undefined
 * when it should show as plain text. `complete` is false while the block is still streaming.
 */
export function highlight(code: string, lang: string, complete: boolean): string | undefined {
  const hljs = api();
  if (!hljs || !code) return undefined;
  const language = lang ? hljs.getLanguage(lang) && lang : undefined;
  if (lang && !language) return undefined;
  if (!complete) return language && code.length <= STREAM_LIMIT ? hljs.highlight(code, { language, ignoreIllegals: true }).value : undefined;

  const key = `${lang}\0${code}`;
  const cached = cache.get(key);
  if (cached !== undefined) return cached ?? undefined;
  let html: string | null = null;
  if (language) html = hljs.highlight(code, { language, ignoreIllegals: true }).value;
  else if (code.length <= AUTO_LIMIT) {
    const result = hljs.highlightAuto(code, AUTO_LANGUAGES);
    // Low relevance means a guess (prose, logs): leave it plain.
    if (result.relevance >= 5) html = result.value;
  }
  if (cache.size >= CACHE_SIZE) cache.delete(cache.keys().next().value!);
  cache.set(key, html);
  return html ?? undefined;
}
