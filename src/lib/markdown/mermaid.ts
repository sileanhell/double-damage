import type { Mermaid } from "mermaid";

export type MermaidResult = { svg: string } | { error: string };

/** mermaid is big: it's loaded (as its own chunk) only when the first finished diagram shows up. */
let loading: Promise<Mermaid> | undefined;
function load(): Promise<Mermaid> {
  loading ??= import("mermaid").then(
    ({ default: mermaid }) => {
      mermaid.initialize({
        startOnLoad: false,
        securityLevel: "strict",
        theme: "dark",
        suppressErrorRendering: true,
        fontFamily: '"Inter", "Segoe UI", system-ui, sans-serif',
        themeVariables: { background: "transparent" },
      });
      return mermaid;
    },
    (error) => {
      loading = undefined; // retry next time
      throw error;
    },
  );
  return loading;
}

const cache = new Map<string, Promise<MermaidResult>>();
const CACHE_SIZE = 64;
let queue: Promise<unknown> = Promise.resolve();
let seq = 0;

const message = (error: unknown) =>
  (error instanceof Error ? error.message : String(error)).split("\n").find((line) => line.trim()) ?? "";

async function draw(source: string): Promise<MermaidResult> {
  let mermaid: Mermaid;
  try {
    mermaid = await load();
  } catch (error) {
    cache.delete(source); // the chunk failed to load: try again on the next render
    return { error: message(error).slice(0, 200) };
  }
  const id = `dd-mermaid-${++seq}`;
  try {
    const { svg } = await mermaid.render(id, source);
    return { svg };
  } catch (error) {
    return { error: message(error).slice(0, 200) };
  } finally {
    // Leftovers mermaid may leave in <body> after a failed render.
    document.getElementById(id)?.remove();
    document.getElementById(`d${id}`)?.remove();
  }
}

/** Renders a diagram to SVG (cached by source; renders run one at a time, as mermaid needs). */
export function renderMermaid(source: string): Promise<MermaidResult> {
  const cached = cache.get(source);
  if (cached) return cached;
  const result = queue.then(() => draw(source));
  queue = result;
  if (cache.size >= CACHE_SIZE) cache.delete(cache.keys().next().value!);
  cache.set(source, result);
  return result;
}
