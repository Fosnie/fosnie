// The production chat/composer; tests replace only HTTP and the WS transport.
import { createRoot } from "react-dom/client";
import { HashRouter,Route,Routes } from "react-router-dom";
import { QueryClient,QueryClientProvider } from "@tanstack/react-query";
import { Chat } from "../../src/screens/Chat";
import { ProjectProvider } from "../../src/app/ProjectContext";
import { WorkmodeProvider } from "../../src/app/WorkmodeContext";
import { loadAuthConfig } from "../../src/auth/config";
import { wsStore } from "../../src/ws/store";
import type { ClientFrame,ServerFrame } from "../../src/ws/protocol";
import { configureInstance } from "../../src/api/instance";
import "../../src/styles/global.css";
import "../../src/styles/design.css";
const handlers=new Set<(frame:ServerFrame)=>void>();
const frames:ClientFrame[]=[];
if(new URL(location.href).searchParams.has("desktop")) {
  Object.assign(window,{__TAURI_INTERNALS__:{invoke:async()=>[],transformCallback:()=>0}});
  configureInstance({baseUrl:location.origin,token:"synthetic-device-token"});
}
wsStore.onFrame=handler=>{handlers.add(handler);return ()=>{handlers.delete(handler);};};
wsStore.send=frame=>{frames.push(frame);};
Object.assign(window,{chatTest:{frames,emit:(frame:ServerFrame)=>handlers.forEach(h=>h(frame))}});
await loadAuthConfig();
createRoot(document.getElementById("root")!).render(<QueryClientProvider client={new QueryClient({defaultOptions:{queries:{retry:false}}})}>
  <WorkmodeProvider><ProjectProvider><HashRouter><Routes>
    <Route path="/c/:chatId" element={<Chat/>}/><Route path="*" element={<Chat/>}/>
  </Routes></HashRouter></ProjectProvider></WorkmodeProvider>
</QueryClientProvider>);
