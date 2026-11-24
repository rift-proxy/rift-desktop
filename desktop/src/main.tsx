import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { BackendProvider } from "./backend/provider";
import { tauriBackend } from "./backend/tauri";
import { ErrorBoundary, installErrorHandlers } from "./components/errors";
import { installWindowGuards } from "./windowGuards";
import "./theme.css";
installErrorHandlers(tauriBackend.logError);
// The inspector and the page menu stay in `pnpm dev`; the installed Rift has neither.
if (!import.meta.env.DEV) installWindowGuards();
ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary logError={tauriBackend.logError}>
      <BackendProvider client={tauriBackend}>
        <App />
      </BackendProvider>
    </ErrorBoundary>
  </React.StrictMode>,
);
