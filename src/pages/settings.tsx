import { Tabs } from "@kobalte/core/tabs";
import { For, type JSX } from "solid-js";
import { BackgroundSettings } from "../components/background-settings";
import { DataDirSettings } from "../components/data-dir-settings";
import { Dropdown } from "../components/dropdown";
import { LlamaServerSettings } from "../components/llama-server-settings";
import { LocalModelsSettings } from "../components/local-models-settings";
import { Toggle } from "../components/toggle";
import { setWidgetsEnabled, widgetsEnabled } from "../lib/features";
import {
  LOCALE_NAMES,
  LOCALES,
  type LocaleSetting,
  localeSetting,
  type MessageKey,
  setLocaleSetting,
  t,
} from "../lib/i18n";

function Section(props: { title: string; children: JSX.Element }) {
  return (
    <section class="mt-8 first:mt-0">
      <h2 class="text-sm font-medium text-white/45">{props.title}</h2>
      <div class="mt-3 rounded-2xl border border-white/10 bg-white/[0.03] p-5">{props.children}</div>
    </section>
  );
}

function LanguageSelect() {
  const options = () => [
    { value: "system" as LocaleSetting, label: t("settings.languageSystem") },
    ...LOCALES.map((code) => ({ value: code as LocaleSetting, label: LOCALE_NAMES[code] })),
  ];
  return (
    <div class="flex items-center justify-between gap-4">
      <div>
        <div class="text-base font-medium">{t("settings.language")}</div>
        <div class="mt-1 text-sm text-white/45">{t("settings.languageHint")}</div>
      </div>
      <Dropdown
        variant="outline"
        placement="bottom-start"
        title={t("settings.language")}
        value={localeSetting()}
        options={options()}
        onChange={setLocaleSetting}
      />
    </div>
  );
}

function WidgetsToggle() {
  return (
    <Toggle
      label={t("settings.widgets")}
      description={t("settings.widgetsHint")}
      checked={widgetsEnabled()}
      onChange={setWidgetsEnabled}
    />
  );
}

function Placeholder() {
  return <p class="text-sm text-white/45">{t("settings.empty")}</p>;
}

const TABS: { id: string; label: MessageKey; content: () => JSX.Element }[] = [
  {
    id: "general",
    label: "settings.tab.general",
    content: () => (
      <div class="rounded-2xl border border-white/10 bg-white/[0.03] p-5">
        <DataDirSettings />
      </div>
    ),
  },
  { id: "modules", label: "settings.tab.modules", content: () => <Placeholder /> },
  {
    id: "interface",
    label: "settings.tab.interface",
    content: () => (
      <>
        <Section title={t("settings.language")}>
          <LanguageSelect />
        </Section>
        <Section title={t("settings.appearance")}>
          <BackgroundSettings />
        </Section>
        <Section title={t("settings.features")}>
          <WidgetsToggle />
        </Section>
      </>
    ),
  },
  {
    id: "providers",
    label: "settings.tab.providers",
    content: () => (
      <div class="rounded-2xl border border-white/10 bg-white/[0.03] p-5">
        <LlamaServerSettings />
      </div>
    ),
  },
  { id: "local-models", label: "settings.tab.localModels", content: () => <LocalModelsSettings /> },
];

export function SettingsPage() {
  return (
    <div class="thin-scroll h-full overflow-y-auto">
      <div class="w-full px-8 pt-8 pb-12">
        <Tabs defaultValue="general">
          <Tabs.List class="relative flex gap-1 border-b border-white/10">
            <For each={TABS}>
              {(tab) => (
                <Tabs.Trigger
                  value={tab.id}
                  class="rounded-t-lg px-3 pb-2.5 pt-1.5 text-sm font-medium text-white/45 outline-none transition-colors hover:text-white/80 focus-visible:ring-2 focus-visible:ring-edge-active data-selected:text-white"
                >
                  {t(tab.label)}
                </Tabs.Trigger>
              )}
            </For>
            <Tabs.Indicator class="absolute -bottom-px h-0.5 rounded-full bg-accent transition-all duration-200" />
          </Tabs.List>

          <For each={TABS}>
            {(tab) => (
              <Tabs.Content value={tab.id} class="mt-8 outline-none">
                {tab.content()}
              </Tabs.Content>
            )}
          </For>
        </Tabs>
      </div>
    </div>
  );
}
