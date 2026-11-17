import { createRoot } from "react-dom/client";
import "./index.css";
import { App } from "./App";
import { engine } from "./engine";

createRoot(document.getElementById("root")!).render(<App />);
// first paint ≈ the task after the first rAF following the initial render
requestAnimationFrame(() => setTimeout(() => (engine.firstPaintWall = Date.now()), 0));

import { invoke } from "@tauri-apps/api/core";
window.addEventListener("error", (e) => invoke("log", { msg: "window.error: " + e.message }));
window.addEventListener("unhandledrejection", (e) => invoke("log", { msg: "rejection: " + e.reason }));
