import { Route, Router as SolidRouter } from "@solidjs/router";

import { HomePage } from "../pages/home";

export function Router() {
  return (
    <SolidRouter>
      <Route path="/" component={HomePage} />
    </SolidRouter>
  );
}
