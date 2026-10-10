// Production Research screen/status, with HTTP replaced only by the browser tests.
import { createRoot } from "react-dom/client";
import { HashRouter, Route, Routes, useParams } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { DeepResearch } from "../../src/screens/DeepResearch";
import { ResearchJobStatus } from "../../src/components/ResearchJob";
import { useChatArtefacts, useChatMessages, useResearchChats } from "../../src/api/client";
import { loadAuthConfig } from "../../src/auth/config";
import "../../src/styles/global.css";
import "../../src/styles/design.css";
function Result() {
  const { id } = useParams();
  const chats = useResearchChats();
  const messages = useChatMessages(id);
  const artefacts = useChatArtefacts(id);
  const jobId = chats.data?.find(c => c.id === id)?.research_params?.routed_job_id;
  return <main style={{ maxWidth: 650, margin: "24px auto" }}>
    {jobId && id && <ResearchJobStatus key={jobId} jobId={jobId} chatId={id} />}
    {messages.data?.map(m => <p key={m.id}>{m.content}</p>)}
    <p>Saved reports: {artefacts.data?.length ?? 0}</p>
  </main>;
}
await loadAuthConfig();
createRoot(document.getElementById("root")!).render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
  <HashRouter><Routes><Route path="/c/:id" element={<Result />} /><Route path="*" element={<DeepResearch />} /></Routes></HashRouter>
</QueryClientProvider>);
