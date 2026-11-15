# Double Damage

Tauri 2 + SolidJS + Tailwind CSS v4 desktop app. Package manager: bun.

## UI components

Build every interactive UI component on [Kobalte](https://kobalte.dev) (`@kobalte/core`, imported per
component, e.g. `@kobalte/core/dialog`) and style it with Tailwind. Do not hand-roll buttons, dialogs,
selects, text fields, popovers, tooltips, etc., and do not add another component library.

Hand-written markup is fine only where Kobalte has no primitive (e.g. the sidebar resize handle).

## Checks

- `bunx tsc` - type check
- `bun run build:web` - type check + production web build
