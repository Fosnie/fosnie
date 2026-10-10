# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
"""Bounded draft verification and repair. No failure becomes a deletion proposal."""
import asyncio
from uuid import UUID
from . import decompose, llm, locate, qdrant_store, rag_ctx, repair, retrieve, verify
from .inference_target import InferenceEnvelope, with_report
from .routed_document import Limits, read


def scope(kbs, denied):
    if not kbs or len(kbs) > 64 or len(denied) > 100000:
        raise ValueError("invalid draft evidence scope")
    for value in [*kbs, *denied]:
        UUID(value)
    budget = rag_ctx.cfg("_routing_budget", None)
    fixed = rag_ctx.cfg("_routing_fixed_budget", None)
    if budget is None or fixed is None or "document" not in budget.grant.allowances:
        raise ValueError("draft requires a shared bounded runtime")
    if not {"embed", "rerank", "verify_claims"} <= set(fixed.grant.allowances):
        raise ValueError("missing draft inference lanes")
    return budget


async def evidence(text, kbs, denied):
    hits = await retrieve._search_one(text, kbs, asyncio.Semaphore(1))
    for hit in hits:
        p = hit.get("payload", {})
        if (p.get("knowledge_base_id") not in kbs or p.get("doc_id") in denied
                or not isinstance(p.get("doc_id"), str) or not isinstance(p.get("chunk_text"), str)):
            raise ValueError("evidence does not belong to the authorized sources")
        UUID(p["doc_id"])
    node = "\n\n".join(h["payload"]["chunk_text"] for h in hits)
    if len(node.encode("utf-8")) > 16384:
        raise ValueError("draft evidence exceeds limit")
    return node, [h["payload"]["doc_id"] for h in hits]


async def verify_draft(path, mime, kbs, denied, strictness):
    budget = scope(kbs, denied)
    if strictness not in ("strict", "lenient"):
        raise ValueError("invalid draft strictness")
    token = qdrant_store.set_deny_docs(denied)
    try:
        async with asyncio.timeout(budget.remaining()):
            with rag_ctx.scoped_overrides({"top_k": 3, "over_retrieval": 1, "verify_claims_batch": 16}):
                result = await read(path, mime, None, Limits(max_file_bytes=4*1024*1024,
                    output_bytes=16000, max_sections=4), terminal=False)
                text = result["text"]
                if result["truncated"] or not text.strip():
                    raise ValueError("draft is empty or exceeds the bounded text limit")
                # Every section is processed. Exceeding the claim cap is an error,
                # never a silently truncated successful verification.
                sections = [text[i:i+4000] for i in range(0, len(text), 4000)]
                claims = []
                for index, section in enumerate(sections):
                    for claim in await decompose.decompose_claims(section):
                        claims.append((index, claim))
                    if len(claims) > 16:
                        raise ValueError("draft exceeds 16 claims")
                nodes = [await evidence(c, kbs, denied) for _, c in claims]
                verdicts = await verify.verify_claims([{"text": c, "evidence": node[0]}
                    for (_, c), node in zip(claims, nodes)])
                counts = dict(supported=0, contradicted=0, not_mentioned=0)
                output = []
                for (index, claim), (node, docs), v in zip(claims, nodes, verdicts):
                    counts[v["verdict"]] += 1
                    output.append(dict(text=claim, **v, evidence=node, evidence_doc_ids=docs,
                        section=f"section {index+1}", had_citation=bool(node.strip()),
                        source_span=locate.locate(claim, text, index*4000)))
                total = len(output)
                n = counts["supported"] + (counts["not_mentioned"] if strictness == "lenient" else 0)
                model = InferenceEnvelope.model_validate(rag_ctx.cfg("routing", None)).fixed_assignments["verify_claims"].model
                return with_report(dict(claims=output, document_text=text, model=model,
                    score=n/total if total else None, total=total, **counts))
    finally:
        qdrant_store.reset_deny_docs(token)


async def repair_draft(claims, kbs, denied, strictness):
    budget = scope(kbs, denied)
    if strictness not in ("strict", "lenient") or not 1 <= len(claims) <= 16:
        raise ValueError("invalid bounded repair request")
    for c in claims:
        if (c.get("verdict") not in ("contradicted", "not_mentioned")
                or any(not isinstance(c.get(k), str) or not c[k].strip() or len(c[k].encode("utf-8")) > 4096
                       for k in ("text", "source_text"))):
            raise ValueError("invalid repair claim")
    token = qdrant_store.set_deny_docs(denied)
    try:
        async with asyncio.timeout(budget.remaining()):
            with rag_ctx.scoped_overrides({"top_k": 3, "over_retrieval": 1, "verify_claims_batch": 16}):
                results = []
                # Owned sequential work bounds fan-out and cancels without stragglers.
                for c in claims:
                    node, docs = await evidence(c["text"], kbs, denied)
                    out = dict(source_text=c["source_text"], claim_text=c["text"], action="cut", replacement=None,
                        evidence=node, evidence_doc_ids=docs, reverify_verdict="not_mentioned", reverify_score=0.0)
                    if node.strip():
                        llm.set_stage("draft.repair")
                        rewritten = (await llm.complete(repair._SYSTEM,
                            f'STATEMENT: {c["source_text"]}\n\nSOURCE: {node}', max_tokens=400)).strip()
                        if not rewritten or len(rewritten.encode("utf-8")) > 4096:
                            raise ValueError("invalid repair model output")
                        if not repair._is_cut(rewritten):
                            v = (await verify.verify_claims([dict(text=rewritten, evidence=node)]))[0]
                            out.update(reverify_verdict=v["verdict"], reverify_score=v["score"])
                            if v["verdict"] == "supported":
                                out["action"] = "kept" if rewritten == c["source_text"] else "regenerated"
                                if out["action"] == "regenerated":
                                    out["replacement"] = rewritten
                    results.append(out)
                return with_report(dict(results=results))
    finally:
        qdrant_store.reset_deny_docs(token)
