/* @refresh reload */
import { getCurrentWindow } from "@tauri-apps/api/window";
import { render } from "solid-js/web";
import "./global.css";
import { Wrapper } from "./wrapper";

render(() => <Wrapper />, document.querySelector("main") as HTMLElement);

getCurrentWindow().show().catch(console.error);
