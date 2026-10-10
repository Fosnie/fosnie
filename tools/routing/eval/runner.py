"""Offline production-engine replay and explicitly budgeted Fosnie live evaluation."""
import argparse
from collections import Counter, defaultdict
from copy import deepcopy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

from corpus import ROOT, VERSION, encoded

CORE = ROOT.parents[2]
TIERS = {"economy": 0, "balanced": 1, "advanced": 2}
STRATEGIES = ("always_economy", "best_single_fixture", "rules", "classifier", "hybrid", "oracle_fixture")


def load_dataset(directory=ROOT / "fixtures"):
    manifest = json.loads((directory / "manifest.json").read_text(encoding="utf-8"))
    payload = (directory / "cases.jsonl").read_bytes()
    if len(payload) > 16_000_000 or hashlib.sha256(payload).hexdigest() != manifest["cases_sha256"]:
        raise ValueError("dataset size/hash mismatch")
    cases = [json.loads(line) for line in payload.splitlines()]
    if manifest["schema_version"] != 1 or not 300 <= len(cases) <= 500 or len(cases) != manifest["case_count"]:
        raise ValueError("dataset version/count mismatch")
    seen, families, groups = set(), {}, {}
    required = {"schema_version", "id", "family_id", "split_group", "split", "category", "language", "difficulty", "data_origin",
                "messages", "context", "tools", "requirements", "objective", "policy_overrides", "constraints", "profile_overrides",
                "classifier_reply", "classifier_failure", "primary_failure", "helpers", "validator", "expected", "mock_answers", "smoke"}
    for case in cases:
        if set(case) != required or case["schema_version"] != 1 or case["data_origin"] != "synthetic" or case["id"] in seen:
            raise ValueError("invalid frozen case identity/schema/origin")
        if case["split"] not in ("train", "calibration", "test") or case["language"] not in ("en", "ru"):
            raise ValueError("invalid split/language")
        for mapping, key in ((families, case["family_id"]), (groups, case["split_group"])):
            if key in mapping and mapping[key] != case["split"]:
                raise ValueError("family/template split leakage")
            mapping[key] = case["split"]
        if len(encoded(case["messages"]).encode()) > 64000:
            raise ValueError("case context exceeds supported text allowance")
        seen.add(case["id"])
    return manifest, cases


def exact_json(value, expected):
    if type(value) is not type(expected):
        return False
    if isinstance(expected, dict):
        return value.keys() == expected.keys() and all(exact_json(value[k], v) for k, v in expected.items())
    if isinstance(expected, list):
        return len(value) == len(expected) and all(exact_json(a, b) for a, b in zip(value, expected))
    return value == expected


def validate_answer(case, content, tool_calls=None):
    if tool_calls:
        return "fail"
    if content is None or case["validator"]["kind"] == "unknown":
        return "unknown"
    if not isinstance(content, str) or len(content.encode()) > 64000:
        return "fail"
    try:
        value = json.loads(content)
        if not isinstance(value, dict):
            return "fail"
        expected = case["validator"]["expected"]
        kind = case["validator"]["kind"]
        if kind == "json_exact":
            return "pass" if exact_json(value, expected) else "fail"
        if kind == "euler_certificate":
            p, a = case["context"][0]["p"], case["context"][0]["a"]
            valid = set(value) == {"p", "a", "euler"} and value["p"] == p and value["a"] == a
            valid = valid and type(value["euler"]) is int and value["euler"] == pow(a, (p - 1) // 2, p) == p - 1
            return "pass" if valid else "fail"
        if kind == "topological_order":
            order = value.get("order")
            graph = case["context"][0]
            valid = set(value) == {"order"} and isinstance(order, list) and all(type(i) is int for i in order)
            valid = valid and sorted(order) == graph["nodes"] and all(order.index(a) < order.index(b) for a, b in graph["edges"])
            return "pass" if valid else "fail"
    except (ValueError, TypeError, KeyError, RecursionError):
        return "fail"
    raise ValueError("unsupported validator")


def replay_input(manifest, case, strategy, provider=None):
    policy = deepcopy(manifest["policies"][strategy])
    policy.update(deepcopy(case["policy_overrides"]))
    policy["objective"] = case["objective"]
    profiles = deepcopy(manifest["profiles"])
    for p in profiles:
        p.update(case["profile_overrides"].get(p["provider_id"], {}))
    key = case["id"] + "/" + (provider or strategy)
    return {"schema_version": 1, "case_id": case["id"], "decision_id": str(uuid.uuid5(uuid.NAMESPACE_URL, VERSION + "/" + key)),
            "policy": policy, "profiles": profiles, "requirements": case["requirements"], "constraints": case["constraints"],
            "classifier_reply": case["classifier_reply"], "classifier_failure": case["classifier_failure"],
            "fixed_provider_id": provider, "primary_failure": case["primary_failure"], "helpers": case["helpers"]}


def run_replay(inputs, binary):
    if not binary.is_file():
        raise ValueError("build backend example routing_replay first, or pass --build")
    payload = "".join(encoded(value) + "\n" for value in inputs)
    completed = subprocess.run([str(binary)], input=payload, text=True, encoding="utf-8", capture_output=True, timeout=60, check=False)
    if completed.returncode:
        raise ValueError("production replay rejected input; no request contents logged")
    results = [json.loads(line) for line in completed.stdout.splitlines()]
    if len(results) != len(inputs) or any(r["case_id"] != i["case_id"] or r["decision_id"] != i["decision_id"] for r, i in zip(results, inputs)):
        raise ValueError("replay result identity/count mismatch")
    return results


def contract_errors(case, request, result, fixed=False):
    errors = []
    constraints = result["constraints"]
    usage = result["ledger"]
    for field, used in (("max_calls", "calls"), ("max_total_tokens", "tokens"), ("max_cost_microusd", "cost_microusd")):
        if constraints.get(field) is not None and usage[used] > constraints[field]:
            errors.append("budget_" + field)
    if constraints.get("max_cost_microusd") is not None and usage["unknown_cost_calls"]:
        errors.append("unknown_cost_under_hard_cap")
    stages = result["stages"]
    if len(stages) != usage["calls"] or sum(s["estimated_cost_microusd"] or 0 for s in stages) != usage["cost_microusd"]:
        errors.append("stage_accounting")
    if sum(s["estimated_cost_microusd"] is None for s in stages) != usage["unknown_cost_calls"]:
        errors.append("unknown_cost_accounting")
    if result["classifier_calls"] > 1 or result["fallback_count"] > 1:
        errors.append("call_ceiling")
    if request["policy"]["strategy"] == "rules" and result["classifier_calls"]:
        errors.append("rules_judge")
    if result["actual_target"] is not None or result["actual_cost_microusd"] is not None or result["model_quality_status"] != "not_run":
        errors.append("fabricated_live_metric")
    profiles = {p["provider_id"]: p for p in request["profiles"]}
    for stage in stages:
        p = profiles.get(stage["provider_id"])
        if p is None:
            errors.append("foreign_stage")
            continue
        if constraints.get("local_only") and (p["locality"] != "local" or not p["locality_trusted"]):
            errors.append("local_only")
        pool = constraints.get("allowed_providers")
        if pool is not None and p["provider_id"] not in pool:
            errors.append("mandatory_pool")
        pool = result["executor_allowed_providers"]
        if not fixed and stage["purpose"] == "primary" and pool is not None and p["provider_id"] not in pool:
            errors.append("executor_pool")
        if stage["purpose"] == "primary":
            req = request["requirements"] if fixed else result["effective_requirements"]
            caps = p["capabilities"]
            if (caps.get("context_tokens") or 0) < req["input_tokens"] + req["output_tokens"] or (caps.get("max_output_tokens") or 0) < req["output_tokens"]:
                errors.append("context_or_output")
            if req.get("effort") and req["effort"] not in caps["efforts"]:
                errors.append("effort")
            if any(req.get(k) and caps.get(k, "unknown") == "unknown" for k in ("tools", "vision", "structured_output")):
                errors.append("capability")
            if not fixed and (not p["auto_enabled"] or p["quality_status"] == "unknown" or TIERS[p["task_tiers"].get(req["task"], p["tier"])] < TIERS[req["minimum_tier"]]):
                errors.append("auto_evidence_or_tier")
    fatal = case["primary_failure"] in ("mid_stream", "cancelled", "unauthorized")
    if not fixed and result["ok"] == bool(case["expected"]["deny_auto"] or fatal):
        errors.append("unexpected_acceptance_or_refusal")
    return sorted(set(errors))


def record(case, request, result, strategy):
    target = result["simulated_target"]
    answer = case["mock_answers"].get(target["provider_id"]) if target and result["ok"] else None
    status = validate_answer(case, answer)
    profile = next((p for p in request["profiles"] if target and p["provider_id"] == target["provider_id"]), None)
    quality_tier = profile["task_tiers"].get(result["effective_requirements"]["task"], profile["tier"]) if profile else None
    errors = contract_errors(case, request, result, request["fixed_provider_id"] is not None)
    return {"schema_version": 1, "mode": "offline_mock", "case_id": case["id"], "family_id": case["family_id"],
            "split_group": case["split_group"], "split": case["split"], "category": case["category"], "language": case["language"],
            "objective": case["objective"], "strategy": strategy, "routing_contract_status": "fail" if errors else "pass",
            "validator_status": status, "model_quality_status": "not_run", "quality_score": None,
            "simulated_target": target, "actual_target": None, "policy_revision": request["policy"]["revision"],
            "estimated_cost_microusd": None if result["ledger"]["unknown_cost_calls"] else result["ledger"]["cost_microusd"],
            "known_cost_subtotal_microusd": result["ledger"]["cost_microusd"], "actual_cost_microusd": None,
            "evaluator_cost_microusd": 0, "classifier_calls": result["classifier_calls"], "fallback_count": result["fallback_count"],
            "stage_usage": result["stages"], "usage": result["ledger"], "latency_ms": None, "ttft_ms": None,
            "under_routing": bool(result["ok"] and quality_tier and TIERS[quality_tier] < TIERS[case["expected"]["minimum_tier"]]),
            "advanced_for_noncritical_task": bool(result["ok"] and quality_tier == "advanced" and case["expected"]["minimum_tier"] == "economy" and case["objective"] != "quality"),
            "errors": errors, "execution_error": result["error"]}


def offline(manifest, cases, binary, verify=False):
    candidates = [p["provider_id"] for p in manifest["profiles"] if p["auto_enabled"] and p["quality_status"] != "unknown"]
    requests, keys = [], []
    for case in cases:
        for strategy, provider in [(s, None) for s in ("rules", "classifier", "hybrid")] + [("rules", p) for p in candidates]:
            requests.append(replay_input(manifest, case, strategy, provider));keys.append((case["id"], provider or strategy))
        if case["primary_failure"]:
            for provider in candidates:
                req = replay_input(manifest, case, "rules", provider)
                req["primary_failure"] = None
                req["decision_id"] = str(uuid.uuid5(uuid.NAMESPACE_URL, VERSION + "/oracle/" + case["id"] + provider))
                requests.append(req);keys.append((case["id"], "oracle/" + provider))
    results = run_replay(requests, binary)
    if verify and results != run_replay(requests, binary):
        raise ValueError("production replay is not reproducible")
    indexed = {key: (req, result) for key, req, result in zip(keys, requests, results)}
    # Select a single fixture model using calibration ONLY, before held-out reporting.
    calibration = [c for c in cases if c["split"] == "calibration"]
    if not calibration:
        raise ValueError("calibration set required for best-single baseline")
    def calibration_score(provider):
        records = [record(c, *indexed[c["id"], provider], "calibration_single") for c in calibration]
        return (-sum(r["validator_status"] == "pass" for r in records), sum(r["estimated_cost_microusd"] if r["estimated_cost_microusd"] is not None else 10**12 for r in records), provider)
    best = min(candidates, key=calibration_score)
    records = []
    for case in cases:
        for strategy in STRATEGIES:
            key = {"always_economy": manifest["ids"]["economy_local"], "best_single_fixture": best}.get(strategy, strategy)
            if strategy == "oracle_fixture":
                choices = []
                for provider in candidates:
                    oracle_key = "oracle/" + provider if case["primary_failure"] else provider
                    req, result = indexed[case["id"], oracle_key]
                    row = record(case, req, result, strategy)
                    if row["validator_status"] == "pass" and not row["under_routing"] and row["estimated_cost_microusd"] is not None:
                        choices.append((row["estimated_cost_microusd"], oracle_key))
                key = min(choices)[1] if choices else best
            records.append(record(case, *indexed[case["id"], key], strategy))
    return records, {"best_single_fixture": best, "selection_split": "calibration", "threshold": 0.75,
                     "production_replay_requests": len(requests), "replay_verified": verify,
                     "threshold_fitted": False, "confidence_is_calibrated_probability": False,
                     "oracle": "post-answer synthetic upper bound; no live model quality measurement"}


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError("evaluation redirect refused")


def live_config(path):
    config = json.loads(path.read_text(encoding="utf-8"))
    required = {"schema_version", "base_url", "auth_env", "total_budget_microusd", "request_budget_microusd", "max_cases",
                "max_duration_ms", "max_output_tokens", "policies", "economy_provider_id", "single_provider_id", "default_policy_id"}
    if set(config) != required or type(config["schema_version"]) is not int or config["schema_version"] != 1:
        raise ValueError("invalid live configuration schema")
    url = urllib.parse.urlsplit(config["base_url"])
    if url.username or url.password or url.query or url.fragment or not url.hostname or (url.scheme != "https" and not (url.scheme == "http" and url.hostname in ("127.0.0.1", "localhost", "::1"))):
        raise ValueError("live URL requires HTTPS or loopback without credentials/query")
    for key in ("total_budget_microusd", "request_budget_microusd", "max_cases", "max_duration_ms", "max_output_tokens"):
        if type(config[key]) is not int or config[key] <= 0:
            raise ValueError("explicit positive integer live limits required")
    if config["request_budget_microusd"] > config["total_budget_microusd"] or config["max_cases"] > 500 or not 50 <= config["max_duration_ms"] <= 30000 or config["max_output_tokens"] > 2048:
        raise ValueError("live limits exceed supported bounds")
    if set(config["policies"]) != {"rules", "classifier", "hybrid"} or not isinstance(config["auth_env"], str) or not config["auth_env"].replace("_", "").isalnum():
        raise ValueError("saved policies and credential environment name required")
    for value in config["policies"].values():
        if set(value) != {"id", "revision"} or type(value["revision"]) is not int or value["revision"] < 1:
            raise ValueError("saved policy ID/revision required")
        uuid.UUID(value["id"])
    for key in ("economy_provider_id", "single_provider_id", "default_policy_id"):
        uuid.UUID(config[key])
    return config


def stratified_select(cases, limit):
    groups = defaultdict(list)
    for case in cases:
        groups[case["category"]].append(case)
    for group in groups.values():
        group.sort(key=lambda c: (c["family_id"], ("economy", "balanced", "quality").index(c["objective"]), c["language"]))
    return [group[i] for i in range(max(map(len, groups.values()), default=0)) for _, group in sorted(groups.items()) if i < len(group)][:limit]


def live(manifest, cases, config, execute=False):
    if not execute:
        return [], {"live_quality": "not_run", "network_calls": 0, "reason": "explicit --execute-live and authorized endpoint/budget required"}
    credential = os.environ.get(config["auth_env"])
    if not credential:
        raise ValueError("live credential environment variable is absent")
    opener = urllib.request.build_opener(NoRedirect)
    def request(path, body=None):
        req = urllib.request.Request(config["base_url"].rstrip("/") + path,
                                     data=encoded(body).encode() if body is not None else None,
                                     headers={"authorization": "Bearer " + credential, "content-type": "application/json"})
        with opener.open(req, timeout=config["max_duration_ms"] / 1000 + 5) as response:
            data = response.read(1_000_001)
            if len(data) > 1_000_000:
                raise ValueError("live response exceeds bound")
            return json.loads(data)
    management = request("/api/routing/management")
    if not management.get("deployment_manage"):
        raise ValueError("evaluation requires effective providers.manage")
    saved = request("/api/routing/policies")
    catalog = request("/api/routing/catalog")
    known = {row["id"]: row for row in saved}
    default = max((p for p in saved if p["is_default"]), key=lambda p: p["scope"] == "user", default=None)
    if default is None or default["id"] != config["default_policy_id"]:
        raise ValueError("manual baselines require the caller's effective default policy")
    for strategy, ref in config["policies"].items():
        if ref["id"] not in known or known[ref["id"]]["revision"] != ref["revision"] or known[ref["id"]]["policy"]["strategy"] != strategy or known[ref["id"]]["policy"]["rollout"] != "active":
            raise ValueError("saved strategy/policy revision mismatch")
    visible = {row["id"] for row in catalog if row["enabled"] and row["profile"] is not None}
    if not {config["economy_provider_id"], config["single_provider_id"]} <= visible:
        raise ValueError("live baseline model is unavailable")
    records, reserved = [], 0
    for case in stratified_select(cases, config["max_cases"]):
        for strategy in ("always_economy", "configured_single", "rules", "classifier", "hybrid"):
            cap = config["request_budget_microusd"]
            if reserved + cap > config["total_budget_microusd"]:
                return records, {"reserved_cost_upper_microusd": reserved, "budget_stop": True, "live_quality": "insufficient_evidence"}
            reserved += cap  # Never refund unknown/failed spend; no request retries.
            if strategy in ("rules", "classifier", "hybrid"):
                ref = config["policies"][strategy]
                selection = {"mode": "auto", "objective": case["objective"], "policy_id": ref["id"]}
            else:
                ref = {"id": default["id"], "revision": default["revision"]}
                selection = {"mode": "manual", "provider_id": config["economy_provider_id" if strategy == "always_economy" else "single_provider_id"]}
            payload = {"evaluation_id": str(uuid.uuid4()), "policy_id": ref["id"], "expected_policy_revision": ref["revision"],
                       "selection": selection, "messages": case["messages"], "max_cost_microusd": cap,
                       "max_duration_ms": config["max_duration_ms"], "max_output_tokens": config["max_output_tokens"],
                       "local_only": case["constraints"].get("local_only", False), "requirements": case["requirements"]}
            start = time.monotonic()
            try:
                response = request("/api/routing/evaluate", payload)
                ok = response.get("ok") is True
            except (urllib.error.URLError, ValueError, OSError):
                response, ok = {}, False
            status = validate_answer(case, response.get("content")) if ok else "unknown"
            records.append({"schema_version": 1, "mode": "live", "case_id": case["id"], "family_id": case["family_id"],
                "split_group": case["split_group"], "split": case["split"], "category": case["category"], "language": case["language"],
                "objective": case["objective"], "strategy": strategy, "validator_status": status,
                "routing_contract_status": "unknown", "model_quality_status": "observed" if ok else "unknown",
                "quality_score": 1 if status == "pass" else 0 if status == "fail" else None,
                "actual_target": response.get("decision", {}).get("actual") if ok else None, "simulated_target": None,
                "policy_revision": ref["revision"], "usage": response.get("usage"), "stage_usage": response.get("stage_usage", []),
                "classifier_ledger": response.get("classifier_ledger"), "classifier_calls": (response.get("classifier_ledger") or {}).get("calls"),
                "estimated_cost_microusd": (response.get("usage") or {}).get("cost_microusd"), "actual_cost_microusd": None,
                "reserved_cost_upper_microusd": cap, "evaluator_cost_microusd": 0, "latency_ms": round((time.monotonic() - start) * 1000),
                "ttft_ms": None, "fallback_count": 0, "under_routing": None, "advanced_for_noncritical_task": None,
                "errors": [] if ok else ["live_request_failed_or_refused"]})
    return records, {"reserved_cost_upper_microusd": reserved, "budget_stop": False, "live_quality": "insufficient_evidence",
                     "single_baseline": "operator-configured, not assumed validated", "actual_cost": "unknown; no invoice/cache pricing reconciliation"}


def summary(records):
    groups = defaultdict(list)
    for row in records:
        for dimension, value in (("strategy", row["strategy"]), ("category", row["category"]), ("language", row["language"]), ("split", row["split"]), ("objective", row["objective"])):
            groups[dimension + ":" + value].append(row)
        groups["category_strategy:" + row["category"] + "/" + row["strategy"]].append(row)
    result = {}
    for key, rows in sorted(groups.items()):
        counts = Counter(r["validator_status"] for r in rows)
        costs = [r["estimated_cost_microusd"] for r in rows]
        result[key] = {"n": len(rows), "pass": counts["pass"], "fail": counts["fail"], "unknown": counts["unknown"],
                       "contract_failures": sum(r["routing_contract_status"] == "fail" for r in rows),
                       "unknown_cost_rows": sum(c is None for c in costs), "estimated_cost_microusd": None if any(c is None for c in costs) else sum(costs),
                       "known_estimated_subtotal_microusd": sum(c or 0 for c in costs), "actual_cost_microusd": None,
                       "classifier_calls": sum(r["classifier_calls"] or 0 for r in rows), "fallbacks": sum(r["fallback_count"] for r in rows),
                       "evaluator_cost_microusd": sum(r.get("evaluator_cost_microusd", 0) for r in rows),
                       "under_routing": sum(r["under_routing"] is True for r in rows),
                       "advanced_for_noncritical_task": sum(r["advanced_for_noncritical_task"] is True for r in rows)}
    return result


def source_versions():
    paths = ("backend/examples/routing_replay.rs", "backend/src/model_routing/engine.rs",
             "backend/src/model_routing/classifier.rs", "backend/src/model_routing/budget.rs",
             "backend/src/model_routing/fallback.rs", "backend/src/model_routing/run.rs",
             "backend/src/http/routing_evaluation.rs", "tools/routing/eval/runner.py", "tools/routing/eval/corpus.py")
    hashes = {p: hashlib.sha256((CORE / p).read_bytes()).hexdigest() for p in paths}
    try:
        head = subprocess.run(["git", "-c", "safe.directory=" + str(CORE), "rev-parse", "HEAD"],
                              cwd=CORE, capture_output=True, text=True, timeout=5, check=False)
        revision = head.stdout.strip() if head.returncode == 0 else None
    except (OSError, subprocess.SubprocessError):
        revision = None
    return {"core_head": revision, "source_sha256": hashes,
            "note": "Source hashes identify working-tree implementation; HEAD alone does not describe uncommitted changes."}


def report(manifest, cases, records, details, directory):
    aggregates = summary(records)
    held_out = summary([r for r in records if r["split"] == "test"])
    modes = {r["mode"] for r in records}
    mode = next(iter(modes)) if len(modes) == 1 else "live_not_run" if not modes else "invalid_mixed"
    comparisons = {}
    test_rows = [r for r in records if r["split"] == "test"]
    baseline_name = "best_single_fixture" if mode == "offline_mock" else "configured_single"
    baseline = {r["case_id"]: r for r in test_rows if r["strategy"] == baseline_name}
    for strategy in ("always_economy", "rules", "classifier", "hybrid", "oracle_fixture"):
        pairs = [(r, baseline[r["case_id"]]) for r in test_rows if r["strategy"] == strategy and r["case_id"] in baseline]
        priced = [(a, b) for a, b in pairs if a["estimated_cost_microusd"] is not None and b["estimated_cost_microusd"] is not None]
        comparisons[strategy] = {"paired_cases": len(pairs), "known_price_pairs": len(priced),
            "estimate_reduction_microusd": sum(b["estimated_cost_microusd"] - a["estimated_cost_microusd"] for a, b in priced),
            "actual_saving_microusd": None, "quality_degradation_95pct_upper": None,
            "quality_gate": "not_run" if mode != "live" else "insufficient_evidence",
            "cost_basis": "synthetic_estimates_including_judge_helpers_retry" if mode == "offline_mock" else "conservative_profile_estimates"}
    value = {"schema_version": 1, "dataset_version": manifest["version"], "dataset_sha256": manifest["cases_sha256"],
             "mode": mode, "case_count": len(cases), "result_count": len(records), "model_quality_gate": "not_run" if mode != "live" else "insufficient_evidence",
             "actual_cost_gate": "not_run", "release_gate": "not_met", "details": details, "aggregates": aggregates,
             "implementation": source_versions(),
             "reproduce": ["SQLX_OFFLINE=true cargo build --offline --manifest-path backend/Cargo.toml --example routing_replay",
                           "python -m unittest discover -s tools/routing/eval -p test_eval.py",
                           "python tools/routing/eval/runner.py offline --output tools/routing/eval/reports/offline --verify-replay",
                           "python tools/routing/eval/runner.py live --output tools/routing/eval/reports/live-not-run"],
             "held_out_aggregates": held_out, "held_out_comparisons": comparisons, "results": records,
             "notes": ["Offline responses/classifications/tariffs are fixtures; no measured model accuracy, latency or savings.",
                       "Unknown is not pass. Missing cost is not zero. Classifier/helpers/retries share runtime spend; evaluator spend is separate.",
                       "Held-out report uses test split; no thresholds are fitted. Template categories and dialogue families never cross splits.",
                       "Live path is bounded text only, with frozen sources/tool results; it does not execute web/RAG/document/tool workflows.",
                       "Provider faults/pool/profile mutations are offline scenarios; live uses saved operator policies/catalog and per-case local-only tightening.",
                       "No API credentials, endpoint request headers, raw prompts or raw upstream response bodies are saved in results."]}
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "report.json").write_text(json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n", encoding="utf-8")
    lines = ["# Fosnie routing evaluation", "", f"Mode: **{mode}**. Dataset `{manifest['version']}`; {len(cases)} cases, {len(records)} results.",
             "", f"Model quality: **{value['model_quality_gate']}**. Actual cost: **not_run**. Release gate: **not_met**.", ""]
    lines.extend("- " + n for n in value["notes"])
    lines += ["", "| Strategy | N | Fixture/validator pass | Fail | Unknown | Contract failures | Known estimate subtotal (µUSD) | Unknown-cost rows | Judge calls | Retries |", "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|"]
    for key, row in aggregates.items():
        if key.startswith("strategy:"):
            lines.append(f"| {key[9:]} | {row['n']} | {row['pass']} | {row['fail']} | {row['unknown']} | {row['contract_failures']} | {row['known_estimated_subtotal_microusd']} | {row['unknown_cost_rows']} | {row['classifier_calls']} | {row['fallbacks']} |")
    lines += ["", "Category, language, objective, split and per-case results are in report.json. Counts above cover every selected split; test-only comparisons are separate from calibration.",
              "No numeric quality-preservation or financial-savings claim is made from this mock dataset."]
    (directory / "report.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"{mode}: {len(cases)} cases, {len(records)} results, {sum(r['routing_contract_status']=='fail' for r in records)} contract failures")
    return value


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("mode", choices=("offline", "live"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--smoke", action="store_true")
    parser.add_argument("--verify-replay", action="store_true")
    parser.add_argument("--build", action="store_true")
    parser.add_argument("--replay-bin", type=Path, default=CORE / "backend/target/debug/examples" / ("routing_replay.exe" if os.name == "nt" else "routing_replay"))
    parser.add_argument("--live-config", type=Path)
    parser.add_argument("--execute-live", action="store_true")
    args = parser.parse_args()
    manifest, cases = load_dataset()
    if args.smoke:
        cases = [c for c in cases if c["smoke"]]
    if args.mode == "offline":
        if args.live_config or args.execute_live:
            raise ValueError("offline mode cannot accept live inputs")
        if args.build:
            subprocess.run(["cargo", "build", "--offline", "--example", "routing_replay"], cwd=CORE / "backend",
                           env={**os.environ, "SQLX_OFFLINE": "true"}, check=True)
        records, details = offline(manifest, cases, args.replay_bin, args.verify_replay)
    elif not args.live_config:
        if args.execute_live:
            raise ValueError("authorized live endpoint/configuration/budget is required")
        records, details = [], {"live_quality": "not_run", "network_calls": 0, "reason": "no authorized live configuration"}
    else:
        config = live_config(args.live_config)
        cases = stratified_select(cases, config["max_cases"])
        records, details = live(manifest, cases, config, args.execute_live)
    result = report(manifest, cases, records, details, args.output)
    return 1 if any(r["routing_contract_status"] == "fail" for r in result["results"]) else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (ValueError, OSError, subprocess.SubprocessError, KeyError) as error:
        print("Routing evaluation stopped: " + type(error).__name__ + "; no request contents or credentials logged", file=sys.stderr)
        sys.exit(2)
