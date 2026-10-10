// Production settings component; only the authenticated HTTP transport is mocked.
import { createRoot } from "react-dom/client";
import { QueryClient,QueryClientProvider } from "@tanstack/react-query";
import { RoutingSettings } from "../../src/components/RoutingSettings";
import { loadAuthConfig } from "../../src/auth/config";
import "../../src/styles/global.css";
import "../../src/styles/design.css";
await loadAuthConfig();
const surface=new URLSearchParams(location.search).get("surface")==="personal" ? "personal" : "deployment";
createRoot(document.getElementById("root")!).render(<QueryClientProvider client={new QueryClient({defaultOptions:{queries:{retry:false,refetchOnWindowFocus:false}}})}>
  <main style={{maxWidth:1000,margin:"0 auto",padding:24}}><RoutingSettings surface={surface}/></main>
</QueryClientProvider>);
