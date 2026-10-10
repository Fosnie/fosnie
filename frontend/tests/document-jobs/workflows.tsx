// Production workflow screen; tests replace HTTP only.
import { createRoot } from "react-dom/client";
import { HashRouter } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Workflows } from "../../src/screens/Workflows";
import { DialogHost, ToastHost } from "../../src/components/dialogs";
import { loadAuthConfig } from "../../src/auth/config";
import "../../src/styles/global.css";
import "../../src/styles/design.css";
const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
await loadAuthConfig();
createRoot(document.getElementById("root")!).render(<QueryClientProvider client={client}><HashRouter>
  <button onClick={() => void client.invalidateQueries({ queryKey: ["workflow"] })}>Refresh saved workflow</button>
  <Workflows /><DialogHost /><ToastHost />
</HashRouter></QueryClientProvider>);
