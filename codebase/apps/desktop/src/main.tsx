import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@fontsource-variable/inter";
import "@fontsource-variable/jetbrains-mono";
import "katex/dist/katex.min.css";
import "@genie/ui/tokens.css";
import "@genie/ui/ui.css";
import "./app.css";
import { detectPlatform } from "./platform";
import { App } from "./state/app";
import { AppContext } from "./ui/context";
import { Root } from "./ui/Root";

async function boot() {
  const platform = await detectPlatform();
  const app = new App(platform);
  window.addEventListener("error", (e) => platform.log("error", String(e.message)));
  window.addEventListener("unhandledrejection", (e) => platform.log("error", String((e.reason as Error)?.message ?? e.reason)));
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <AppContext.Provider value={app}>
        <Root />
      </AppContext.Provider>
    </StrictMode>,
  );
  await app.start();
}

void boot();
