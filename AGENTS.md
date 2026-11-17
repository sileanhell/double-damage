# Double Damage

Tauri 2 + SolidJS + Tailwind CSS v4 desktop app. Package manager: bun.

## UI components

Build every interactive UI component on [Kobalte](https://kobalte.dev) (`@kobalte/core`, imported per
component, e.g. `@kobalte/core/dialog`) and style it with Tailwind. Do not hand-roll buttons, dialogs,
selects, text fields, popovers, tooltips, etc., and do not add another component library.

Hand-written markup is fine only where Kobalte has no primitive (e.g. the sidebar resize handle).

## Layout

- `core/` - `dd-core`: providers (llama-server, Claude, Devin), sign-ins (`<data>/core/credentials.toml`),
  local models and the Anthropic/OpenAI-compatible API plus `/dd/*` management routes. A CLI
  (`dd-core serve`, `dd-core login claude`, `dd-core load <url>` …); no Tauri dependency.
- `src-tauri/` - the desktop shell. Starts `dd-core.exe` (next to `dd.exe`, or `DD_CORE_EXE`) and the
  page talks to it over HTTP (`src/lib/core.ts`).

## Checks

- `cargo test -p dd-core` - core tests
- `bunx tsc` - type check
- `bun run build:web` - type check + production web build
