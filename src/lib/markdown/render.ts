import { Marked, type Token, type Tokens } from "marked";

/**
 * Markdown -> blocks for chat replies. The text is untrusted model output, so:
 * - raw HTML is never passed through: block and inline HTML render as escaped text;
 * - links and images become `data-href` spans only for http(s)/mailto URLs (anything else is plain
 *   text), so nothing in the output can navigate the webview or run script;
 * - images are never loaded: they render as a chip with the alt text / URL.
 */

const ESCAPES: Record<string, string> = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
export const escapeHtml = (text: string) => text.replace(/[&<>"']/g, (ch) => ESCAPES[ch]);
/** Like escapeHtml, but keeps entity references (`&lt;`, `&#169;`) so they show as their character. */
const escapeText = (text: string) => text.replace(/&(?!#?\w+;)|[<>"']/g, (ch) => ESCAPES[ch]);

const SAFE_PROTOCOLS = new Set(["http:", "https:", "mailto:"]);

/** The normalized URL if it may be offered to the user (http, https, mailto), else undefined. */
export function safeUrl(href: string | null | undefined): string | undefined {
  if (!href) return undefined;
  let url: URL;
  try {
    url = new URL(href.trim());
  } catch {
    return undefined;
  }
  if (!SAFE_PROTOCOLS.has(url.protocol)) return undefined;
  if (url.protocol !== "mailto:" && !url.hostname) return undefined;
  return url.href;
}

const marked = new Marked({
  gfm: true,
  breaks: false,
  renderer: {
    html(token) {
      const text = escapeHtml(token.text);
      return token.type === "html" && token.block ? `<p class="md-raw">${text.replace(/\n+$/, "")}</p>` : text;
    },
    text(token) {
      if ("tokens" in token && token.tokens) return this.parser.parseInline(token.tokens);
      return "escaped" in token && token.escaped ? token.text : escapeText(token.text);
    },
    codespan({ text }) {
      return `<code>${escapeHtml(text)}</code>`;
    },
    // Code blocks are rendered by the component; this only runs for code nested in lists/quotes.
    code({ text }) {
      return `<pre class="md-pre"><code>${escapeHtml(text.replace(/\n$/, ""))}</code></pre>`;
    },
    link({ href, tokens }) {
      const inner = this.parser.parseInline(tokens);
      const url = safeUrl(href);
      if (!url) return inner;
      return `<a class="md-link" role="link" tabindex="0" data-href="${escapeHtml(url)}" title="${escapeHtml(url)}">${inner}</a>`;
    },
    image({ href, text }) {
      const url = safeUrl(href);
      const label = escapeHtml(text || url || href || "");
      if (!url) return `<span class="md-image">${label}</span>`;
      return `<span class="md-image md-link" role="link" tabindex="0" data-href="${escapeHtml(url)}" title="${escapeHtml(url)}">${label}</span>`;
    },
    checkbox({ checked }) {
      return `<span class="md-task${checked ? " md-task-done" : ""}" aria-hidden="true"></span>`;
    },
    table(token) {
      const cell = (c: Tokens.TableCell, tag: "th" | "td") =>
        `<${tag}${c.align ? ` style="text-align:${c.align}"` : ""}>${this.parser.parseInline(c.tokens)}</${tag}>`;
      const head = `<tr>${token.header.map((c) => cell(c, "th")).join("")}</tr>`;
      const body = token.rows.map((row) => `<tr>${row.map((c) => cell(c, "td")).join("")}</tr>`).join("");
      return `<div class="md-table thin-scroll"><table><thead>${head}</thead>${body ? `<tbody>${body}</tbody>` : ""}</table></div>`;
    },
  },
});

export type Block =
  | { kind: "html"; raw: string; html: string }
  | { kind: "code"; raw: string; lang: string; code: string; closed: boolean }
  | { kind: "mermaid"; raw: string; code: string; closed: boolean };

const FENCE = /^ {0,3}(`{3,}|~{3,})/;

/** Whether a fenced code token's raw text ends with its closing fence (indented code: always). */
function fenceClosed(raw: string): boolean {
  const open = FENCE.exec(raw);
  if (!open) return true;
  const lines = raw.replace(/\n+$/, "").split("\n");
  if (lines.length < 2) return false;
  const close = new RegExp(`^ {0,3}${open[1][0] === "`" ? "`" : "~"}{${open[1].length},}[ \\t]*$`);
  return close.test(lines[lines.length - 1]);
}

function toBlock(token: Token): Block {
  if (token.type === "code") {
    const code = token as Tokens.Code;
    const closed = fenceClosed(code.raw);
    let text = code.text;
    // Mid-stream "``" of the closing fence: don't flash it as code.
    if (!closed) text = text.replace(/(^|\n) {0,3}(`{1,2}|~{1,2})$/, "");
    const lang = (code.lang ?? "").trim().split(/\s+/)[0].toLowerCase();
    if (lang === "mermaid") return { kind: "mermaid", raw: code.raw, code: text, closed };
    return { kind: "code", raw: code.raw, lang, code: text, closed };
  }
  return { kind: "html", raw: token.raw, html: marked.parser([token]) };
}

/** Top-level tokens of `text` (CRLF already normalized). */
const lex = (text: string): Token[] => [...marked.lexer(text)];

/** A link reference definition anywhere: links in earlier blocks may depend on it. */
const DEFINITION = /^ {0,3}\[[^\]\n]+\]:/m;
/** How many trailing non-space blocks are re-lexed when text was only appended. */
const RELEX = 2;

/**
 * Splits markdown into top-level blocks, incrementally. Returns a function to call with each new
 * version of the same message:
 * - blocks whose source didn't change come back as the same objects, so an indexed list leaves
 *   their DOM alone (while streaming, only the last block changes);
 * - when the text only grew, just the last couple of blocks are lexed again instead of the whole
 *   text (unless it has link reference definitions, which can affect any block).
 */
export function blockParser(): (text: string) => Block[] {
  let prevText = "";
  let prevTokens: Token[] = [];
  let prevBlocks: Block[] = [];

  return (input) => {
    const text = input.replace(/\r\n?/g, "\n");
    let tokens: Token[] | undefined;
    if (prevTokens.length && text.startsWith(prevText) && !DEFINITION.test(text)) {
      // Back off to the start of the RELEX-th last non-space token.
      let cut = prevTokens.length;
      for (let seen = 0; cut > 0 && seen < RELEX; ) if (prevTokens[--cut].type !== "space") seen++;
      if (cut > 0) {
        const kept = prevTokens.slice(0, cut);
        let offset = 0;
        for (const token of kept) offset += token.raw.length;
        tokens = kept.concat(lex(text.slice(offset)));
      }
    }
    tokens ??= lex(text);
    // Raw texts must tile the input exactly; otherwise fall back to a full lex.
    let length = 0;
    for (const token of tokens) length += token.raw.length;
    if (length !== text.length) tokens = lex(text);

    const blocks: Block[] = [];
    for (const token of tokens) {
      if (token.type === "space" || token.type === "def") continue;
      const old = prevBlocks[blocks.length];
      blocks.push(old && old.raw === token.raw ? old : toBlock(token));
    }
    prevText = text;
    prevTokens = tokens;
    prevBlocks = blocks;
    return blocks;
  };
}

/** One-shot: the blocks of a finished text. */
export const toBlocks = (text: string): Block[] => blockParser()(text);
