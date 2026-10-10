// Real HTTP/WS transport and product composer. Identity is supplied by the
// loopback-only Rust acceptance fixture, never by the deployed application.
import { createRoot } from "react-dom/client";
import { HashRouter, Route, Routes } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Chat } from "../../src/screens/Chat";
import { ProjectProvider } from "../../src/app/ProjectContext";
import { WorkmodeProvider } from "../../src/app/WorkmodeContext";
import { loadAuthConfig } from "../../src/auth/config";
import { wsStore } from "../../src/ws/store";
import "../../src/styles/global.css";
import "../../src/styles/design.css";

await loadAuthConfig();
wsStore.start();
window.addEventListener("pagehide", () => wsStore.stop());
createRoot(document.getElementById("root")!).render(
  <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <WorkmodeProvider><ProjectProvider><HashRouter><Routes>
      <Route path="/c/:chatId" element={<Chat />} /><Route path="*" element={<Chat />} />
    </Routes></HashRouter></ProjectProvider></WorkmodeProvider>
  </QueryClientProvider>,
);
