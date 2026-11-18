import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { render } from "preact";
import { applyAppearance, settingsColors, type Appearance } from "../appearance";
import { App } from "./App";
import "./settings.css";

// The window follows the theme (never see-through: Glass shows as dark).
const themed = (a: Appearance) => applyAppearance(a, settingsColors(a), document.documentElement);
listen<Appearance>("appearance:changed", (e) => themed(e.payload)).catch(() => {});
invoke<Appearance>("appearance").then(themed, () => {});

render(<App />, document.getElementById("app")!);
