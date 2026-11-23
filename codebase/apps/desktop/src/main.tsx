import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
// Poppins for all UI text (static weights: regular, medium, semibold, bold, plus italic).
import "@fontsource/poppins/400.css";
import "@fontsource/poppins/400-italic.css";
import "@fontsource/poppins/500.css";
import "@fontsource/poppins/600.css";
import "@fontsource/poppins/700.css";
import "@fontsource-variable/jetbrains-mono";
import "katex/dist/katex.min.css";
import "@genie/ui/tokens.css";
import "@genie/ui/ui.css";
import "./app.css";
import { UiProvider } from "@genie/ui";
import { detectPlatform } from "./platform";
import { App } from "./state/app";
import { AppContext } from "./ui/context";
import { Root } from "./ui/Root";

// Host OS from the webview itself (the engine's `os` is "mock" in the browser preview), so
// CSS can size type for Windows' thinner font rendering from the first paint.
if (/Windows/i.test(navigator.userAgent)) document.documentElement.dataset.platform = "windows";

async function boot() {
  const platform = await detectPlatform();
  const app = new App(platform);
  window.addEventListener("error", (e) => platform.log("error", String(e.message)));
  window.addEventListener("unhandledrejection", (e) => platform.log("error", String((e.reason as Error)?.message ?? e.reason)));
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <AppContext.Provider value={app}>
        <UiProvider>
          <Root />
        </UiProvider>
      </AppContext.Provider>
    </StrictMode>,
  );
  await app.start();
}

void boot();
