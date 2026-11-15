import { I18nProvider } from "@kobalte/core/i18n";
import { LoadingScreen } from "../components/loading-screen";
import { locale } from "../lib/i18n";
import { Router } from "./router";

export function Wrapper() {
  return (
    // Keeps Kobalte's own locale (number formatting, built-in aria strings) in sync with ours.
    <I18nProvider locale={locale()}>
      <Router />
      <LoadingScreen />
    </I18nProvider>
  );
}
