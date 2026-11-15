/* @refresh reload */
import { getCurrentWindow } from "@tauri-apps/api/window";
import { render } from "solid-js/web";
import "./global.css";
import { Wrapper } from "./wrapper";

const appWindow = getCurrentWindow();
const syncTitle = () => appWindow.setTitle(document.title).catch(console.error);
syncTitle();
new MutationObserver(syncTitle).observe(document.head, { subtree: true, childList: true, characterData: true });

render(() => <Wrapper />, document.querySelector("main") as HTMLElement);

appWindow.show().catch(console.error);
