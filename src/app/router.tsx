import { HashRouter, Route } from "@solidjs/router";

import { HomePage } from "../pages/home";
import { SettingsPage } from "../pages/settings";
import { AppLayout } from "./layout";

// Hash routing: the Tauri webview serves a single index.html, so path routes would 404 on reload.
export function Router() {
  return (
    <HashRouter root={AppLayout}>
      <Route path="/" component={HomePage} />
      <Route path="/settings" component={SettingsPage} />
    </HashRouter>
  );
}
