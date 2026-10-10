// Isolated browser harness: the production component and request layer, mocked HTTP in Playwright.
import { createRoot } from "react-dom/client";
import { useState } from "react";
import { BrowserRouter, useSearchParams } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { DocumentVerificationJob } from "../../src/components/DocumentVerificationJob";
import { useDocumentJobCapabilities } from "../../src/api/documentJobs";
import { loadAuthConfig } from "../../src/auth/config";
import "../../src/styles/global.css";
import "../../src/styles/design.css";
function Harness() {
  const [search, setSearch] = useSearchParams();
  const caps = useDocumentJobCapabilities();
  const [, setResult] = useState<string | null>(null);
  return <main style={{ width: "min(100%, 380px)", margin: "24px auto" }}>
    <DocumentVerificationJob key={search.get("verificationJob") ?? "new"} documentId="document-one" jobId={search.get("verificationJob")}
      targetType={search.get("target") === "draft" ? "draft" : "document"}
      capabilities={caps.isError ? undefined : caps.data} docx repairEnabled disabled={false} onResult={setResult}
      onJob={id => setSearch(prev => { const next = new URLSearchParams(prev); next.set("verificationJob", id); return next; })} />
  </main>;
}
await loadAuthConfig();
createRoot(document.getElementById("root")!).render(<QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
  <BrowserRouter><Harness /></BrowserRouter>
</QueryClientProvider>);
