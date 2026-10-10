// Actual product components, with a loopback demo shell only.
import { createRoot } from "react-dom/client";
import { HashRouter, Route, Routes, Link } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Chat } from "../../src/screens/Chat";
import { RoutingSettings } from "../../src/components/RoutingSettings";
import { ProjectProvider } from "../../src/app/ProjectContext";
import { WorkmodeProvider } from "../../src/app/WorkmodeContext";
import { loadAuthConfig } from "../../src/auth/config";
import { wsStore } from "../../src/ws/store";
import "../../src/styles/global.css";
import "../../src/styles/design.css";

const info = await (await fetch("/demo/info")).json();
localStorage.setItem("pai.defaultAgentId", info.agent_id);
await loadAuthConfig(); wsStore.start();
window.addEventListener("pagehide", () => wsStore.stop());
createRoot(document.getElementById("root")!).render(
  <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <WorkmodeProvider><ProjectProvider><HashRouter>
      <div style={{display:"flex",flexDirection:"column",height:"100vh"}}>
        <header style={{padding:"8px 16px",display:"flex",gap:20,alignItems:"center",borderBottom:"1px solid #888"}}>
          <strong>Fosnie — локальное демо</strong><Link to="/">Чат</Link><Link to="/routing">Модели и маршрутизация</Link>
          <span style={{fontSize:12}}>Qwen 4B / Llama 3B + GPT Mini / Gemini Lite · Quick web demo через меню агента · облако: 12 вызовов / 15 минут</span>
        </header>
        <main style={{flex:1,minHeight:0,overflow:"auto"}}><Routes>
          <Route path="/routing" element={<div style={{padding:20}}><RoutingSettings surface="deployment" /></div>} />
          <Route path="/c/:chatId" element={<Chat />} /><Route path="*" element={<Chat />} />
        </Routes></main>
      </div>
    </HashRouter></ProjectProvider></WorkmodeProvider>
  </QueryClientProvider>,
);
