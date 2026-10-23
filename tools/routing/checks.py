"""Shared PowerShell/POSIX routing check queue. No live inference or publication."""
import argparse
from dataclasses import dataclass
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import urllib.parse

CORE = Path(__file__).resolve().parents[2]
EE = CORE.parent / "fosnie-enterprise"
PYTHON = sys.executable
NPM = "npm.cmd" if os.name == "nt" else "npm"
ML_TESTS = (
    "inference_target inference_budget inference_fixed inference_stages inference_failure routed_retrieval routed_web "
    "routed_document routed_verify routed_draft routed_research routed_research_triage routed_research_quality "
    "research_pipeline provider_overrides reasoning_effort complete_reasoning provider_test anthropic_adapter gemini_adapter "
    "provider_failure_details snippet_search web_quick_evidence"
).split()

@dataclass
class Check:
    name: str
    cwd: Path
    args: list
    db: bool = False
    enterprise: bool = False
    env: dict | None = None

def queue(lane, enterprise=False):
    checks = {
        "protocol": Check("protocol", CORE / "crates/fosnie-protocol", ["cargo", "test", "--offline"]),
        "unit": Check("unit", CORE / "backend", ["cargo", "test", "--offline", "--lib", "model_routing"]),
        "research-unit": Check("research-unit", CORE / "backend", ["cargo", "test", "--offline", "--lib", "research::jobs::tests"]),
        "build": Check("build", CORE / "backend", ["cargo", "check", "--offline", "--all-targets"]),
        "core-isolation": Check("core-isolation", CORE, [PYTHON, "tools/routing/core_isolation.py"]),
        "integration": Check("integration", CORE / "backend", ["cargo", "test", "--offline", "--test", "model_routing_schema", "--test", "provider_registry_seam", "--", "--nocapture", "--test-threads=1"], db=True),
        "classifier": Check("classifier", CORE / "backend", ["cargo", "test", "--offline", "--test", "model_routing_classifier", "--", "--nocapture"], db=True),
        "provider-failures": Check("provider-failures", CORE / "backend", ["cargo", "test", "--offline", "--test", "model_routing_failures", "--", "--nocapture"], db=True),
        "fallback": Check("fallback", CORE / "backend", ["cargo", "test", "--offline", "--test", "model_routing_fallback", "--", "--nocapture"], db=True),
        "durable": Check("durable", CORE / "backend", ["cargo", "test", "--offline", "--lib", "durable_stream_checkpoints", "--", "--ignored", "--nocapture"], db=True),
        "pcm": Check("pcm", CORE / "backend", ["cargo", "test", "--offline", "--test", "voice_routed_pcm"]),
        "ml": Check("ml", CORE / "ml", [PYTHON, "-m", "pytest", "-q", *["tests/test_" + t + ".py" for t in ML_TESTS]]),
        "ml-artefact": Check("ml-artefact", CORE / "ml", [PYTHON, "-m", "pytest", "-q", "tests/test_routed_artefact.py"]),
        "eval-build": Check("eval-build", CORE / "backend", ["cargo", "build", "--offline", "--example", "routing_replay"]),
        "evidence-build": Check("evidence-build", CORE / "backend", ["cargo", "build", "--offline", "--example", "routing_evidence"]),
        "eval-test": Check("eval-test", CORE / "tools/routing/eval", [PYTHON, "-m", "unittest", "-v", "test_eval", "test_judge_diagnostics", "test_judge_review", "test_cohort_report", "test_answer_diagnostics", "test_reference_audit", "test_audited_run", "test_cloud_allocation", "test_reasoning_allocation", "test_unified_pilot", "test_instruction_following", "test_instruction_pilot", "test_request_timing", "test_semantic_review", "test_semantic_judge", "test_semantic_catalog_judge", "test_semantic_cardinality", "test_objective_answers", "test_qa_span_answers", "test_qa_span_diagnostics", "test_source_reference_audit", "test_source_reference_review"]),
        "eval-replay": Check("eval-replay", CORE, [PYTHON, "tools/routing/eval/runner.py", "offline", "--output", "tools/routing/eval/reports/offline", "--verify-replay"]),
        "live-not-run": Check("live-not-run", CORE, [PYTHON, "tools/routing/eval/runner.py", "live", "--output", "tools/routing/eval/reports/live-not-run"]),
        "frontend-unit": Check("frontend-unit", CORE / "frontend", [NPM, "test"]),
        "frontend-core": Check("frontend-core", CORE / "frontend", [NPM, "run", "build"], env={"EDITION_ENTRY": None}),
        "browser": Check("browser", CORE / "frontend", ["node", "node_modules/@playwright/test/cli.js", "test", "-c", "playwright.document-jobs.config.ts", "chat.spec.ts", "routing-settings.spec.ts", "routing-builder.spec.ts"]),
        "stack": Check("stack", CORE / "backend", ["cargo", "test", "--offline", "--test", "model_routing_stack", "--", "--nocapture"], db=True),
        "native": Check("native", CORE / "desktop/src-tauri", ["cargo", "check", "--offline", "--all-targets"]),
        "native-unsigned": Check("native-unsigned", CORE / "desktop", [NPM, "run", "build:unsigned"],
                                 env={"CARGO_NET_OFFLINE": "true", "EDITION_ENTRY": None,
                                      "TAURI_SIGNING_PRIVATE_KEY": None, "TAURI_SIGNING_PRIVATE_KEY_PASSWORD": None}),
        "ee-build": Check("ee-build", EE, ["cargo", "check", "--offline", "--all-targets"], enterprise=True),
        "ee-unit": Check("ee-unit", EE, ["cargo", "test", "--offline", "--lib"], enterprise=True),
        "ee-integration": Check("ee-integration", EE, ["cargo", "test", "--offline", "--test", "model_routing", "--test", "license", "--test", "routed_moderation", "--", "--nocapture", "--test-threads=1"], db=True, enterprise=True),
        "ee-audit": Check("ee-audit", EE, ["cargo", "test", "--offline", "--test", "audit_chain", "--test", "edition_gating", "--test", "edition_gating_phase2", "--", "--nocapture", "--test-threads=1"], db=True, enterprise=True, env={"DATABASE_URL": "isolated", "PAI__REDIS_URL": "redis://127.0.0.1:16389"}),
        "ee-ml": Check("ee-ml", CORE / "ml", [PYTHON, "-m", "pytest", "-q", str(EE / "ml/tests/test_routed_classification.py")], enterprise=True),
        "frontend-ee": Check("frontend-ee", CORE / "frontend", [NPM, "run", "build"], enterprise=True, env={"EDITION_ENTRY": "../../fosnie-enterprise/frontend/index.tsx"}),
    }
    # First-shipment acceptance uses loopback fixtures; quality research is separate.
    checks.update({
        "vnext-harness": Check("vnext-harness", CORE / "tools/routing", [PYTHON, "-m", "unittest", "-v", "test_checks"]),
        "vnext-catalogue": Check("vnext-catalogue", CORE / "backend",
            ["cargo", "test", "--offline", "--test", "model_routing_vnext_catalogue", "--", "--nocapture"], db=True),
        "vnext-ml": Check("vnext-ml", CORE / "ml", [PYTHON, "-m", "pytest", "-q", *[
            "tests/test_" + name + ".py" for name in ("variant_generation", "native_replay", "provider_failure_details", "inference_failure")]]),
        "vnext-frontend": Check("vnext-frontend", CORE / "frontend", [NPM, "test", "--", *[
            "src/api/" + name + ".test.ts" for name in ("routingCatalogue", "routingVNext", "chatRouting", "routingSettings", "routingFailure")]]),
        "acceptance-inference": Check("acceptance-inference", CORE / "backend",
            ["cargo", "test", "--offline", "--test", "model_routing_classifier", "--test", "model_routing_fallback", "--", "--nocapture", "--test-threads=1"], db=True),
        "acceptance-ml": Check("acceptance-ml", CORE / "ml", [PYTHON, "-m", "pytest", "-q", *[
            "tests/test_" + name + ".py" for name in ("inference_target", "inference_budget", "inference_fixed",
                "inference_stages", "inference_failure", "reasoning_effort", "complete_reasoning", "native_replay", "output_schema", "provider_failure_details", "snippet_search", "web_quick_evidence")]]),
        "acceptance-frontend": Check("acceptance-frontend", CORE / "frontend",
            [NPM, "test", "--", "src/api/chatRouting.test.ts", "src/api/routingSettings.test.ts", "src/api/routingFailure.test.ts"]),
        "acceptance-types": Check("acceptance-types", CORE / "frontend", ["node", "node_modules/typescript/bin/tsc", "-b"]),
        "acceptance-ee": Check("acceptance-ee", EE,
            ["cargo", "test", "--offline", "--test", "model_routing", "--test", "model_routing_effort_capabilities", "--", "--nocapture", "--test-threads=1"], db=True, enterprise=True),
    })
    evaluation = ["eval-build", "eval-test", "eval-replay", "live-not-run"]
    ee = ["ee-build", "ee-unit", "ee-integration", "ee-audit", "ee-ml", "frontend-ee"]
    lanes = {
        # D1–D5 consolidated verification. Real Enterprise is mandatory here;
        # Profiles and native dispatch are checked with local fixtures; no paid qualification is required.
        "vnext": ["vnext-harness", "protocol", "build", "core-isolation", "unit", "vnext-catalogue",
            "integration", "classifier", "fallback", "provider-failures", "durable", "stack",
            "ml", "vnext-ml", "vnext-frontend", "acceptance-types", "frontend-core", "browser",
            "ee-build", "acceptance-ee", "ee-ml", "frontend-ee"],
        "acceptance": ["protocol", "unit", "acceptance-inference", "provider-failures", "management-probe", "acceptance-ml",
            "acceptance-frontend", "acceptance-types", "browser", "stack", *(["acceptance-ee"] if enterprise else [])],
        "fast": ["protocol", "unit", "research-unit"],
        "integration": ["integration", "provider-failures", "durable"], "ml": ["ml"],
        "classifier": ["classifier", "fallback", "unit", "management-probe"],
        "fallback": ["fallback", "provider-failures", "unit", "alias", "ml"],
        "quality": ["build", "unit", "fallback", "evidence-build", "frontend-unit", "frontend-core", "browser", *( ["ee-build", "ee-integration", "frontend-ee"] if enterprise else [])],
        "eval-offline": evaluation, "e2e": ["stack", "browser"], "native": ["native"], "native-release": ["native-unsigned"],
        "enterprise": ["ee-build"], "enterprise-routing": ["ee-moderation", "ee-ml"],
        "enterprise-acceptance": ee,
        "all": ["protocol", "build", "core-isolation", "unit", "research-unit", "integration", "classifier", "fallback", "provider-failures", "evidence-build", "durable", "pcm", "ml", *evaluation,
                "frontend-unit", "frontend-core", "stack", "browser", "ml-artefact", *(ee if enterprise else [])],
    }
    for key, test in (("management-probe", "r08_routing_management_and_bounded_judge_probe"), ("alias", "r08_public_api_alias_collision_and_execution")):
        checks[key] = Check(key, CORE / "backend", ["cargo", "test", "--offline", "--test", "model_routing_schema", test, "--", "--nocapture"], db=True)
    checks["ee-moderation"] = Check("ee-moderation", EE, ["cargo", "test", "--offline", "--lib", "--test", "routed_moderation", "moderation", "--", "--nocapture"], db=True, enterprise=True)
    return [checks[k] for k in dict.fromkeys(lanes[lane])]

def validate_db(value):
    url = urllib.parse.urlsplit(value or "")
    if url.scheme not in ("postgres", "postgresql") or url.hostname not in ("127.0.0.1", "localhost", "::1") or not url.path.lstrip("/").startswith("routing_"):
        raise ValueError("A loopback isolated routing_ database is required; no production fallback")

def run(checks, output, environment, merge=False):
    # Fail missing prerequisites before spending time compiling other checks.
    for check in checks:
        if check.enterprise and not (EE / "Cargo.toml").is_file():
            raise ValueError("Real Enterprise checkout unavailable; required lane cannot skip")
        if check.db:
            validate_db(environment.get("FOSNIE_ROUTING_TEST_DATABASE_URL"))
    output.mkdir(parents=True, exist_ok=True)
    results = json.loads((output / "checks.json").read_text(encoding="utf-8"))["checks"] if merge and (output / "checks.json").is_file() else []
    for check in checks:
        if check.enterprise and not (EE / "Cargo.toml").is_file():
            raise ValueError("Real Enterprise checkout unavailable; required lane cannot skip")
        if check.db:
            validate_db(environment.get("FOSNIE_ROUTING_TEST_DATABASE_URL"))
        env = dict(environment)
        for key, value in (check.env or {}).items():
            if value is None: env.pop(key, None)
            else: env[key] = env["FOSNIE_ROUTING_TEST_DATABASE_URL"] if value == "isolated" else value
        print("Routing check: " + check.name, flush=True)
        started = time.monotonic();lines = []
        with (output / (check.name + ".log")).open("w", encoding="utf-8") as log:
            process = subprocess.Popen(check.args, cwd=check.cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, encoding="utf-8", errors="replace")
            try:
                for line in process.stdout:
                    log.write(line);log.flush();print(line, end="", flush=True);lines.append(line)
                code = process.wait()
            except BaseException:
                process.terminate();process.wait();raise
        matches = re.findall(r"test result: .*? (\d+) passed; (\d+) failed; (\d+) ignored", "".join(lines))
        results = [r for r in results if r["name"] != check.name]
        results.append({"name": check.name, "status": "pass" if code == 0 else "failed", "exit_code": code,
                        "seconds": round(time.monotonic() - started, 2),
                        "rust_passed": sum(int(m[0]) for m in matches), "rust_ignored": sum(int(m[2]) for m in matches)})
        (output / "checks.json").write_text(json.dumps({"checks": results, "live_quality": "not_run", "paid_inference": False}, indent=2) + "\n", encoding="utf-8")
        if code: return code
    passed = sum(r["status"] == "pass" for r in results)
    print(f"Requested checks passed: {len(checks)}; accumulated receipts: {passed} passed, {len(results) - passed} failed; Rust test executions {sum(r['rust_passed'] for r in results)}, ignored {sum(r['rust_ignored'] for r in results)} (not counted as passes)", flush=True)
    return 0

def main():
    # Windows redirected output may otherwise use a legacy code page while
    # Cargo/Vitest/Playwright emit Unicode. Logs and the console use one codec.
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    parser = argparse.ArgumentParser()
    parser.add_argument("--lane", default="fast", choices=("vnext", "fast", "integration", "ml", "classifier", "fallback", "quality", "eval-offline", "e2e", "all", "native", "native-release", "enterprise", "enterprise-routing", "enterprise-acceptance", "acceptance"))
    parser.add_argument("--with-enterprise", action="store_true")
    parser.add_argument("--only", help="Execute one named failed/affected check without repeating its whole lane")
    parser.add_argument("--start-at", help="Continue the untouched remainder of a queue after repairing its failed check")
    parser.add_argument("--output", type=Path, default=CORE / "tools/routing/reports/checks")
    parser.add_argument("--list", action="store_true", help="List queue without executing commands or contacting services")
    args = parser.parse_args();checks = queue(args.lane, args.with_enterprise)
    if args.start_at:
        names = [c.name for c in checks]
        if args.start_at not in names or args.only: raise ValueError("start-at requires a queue name and no only filter")
        checks = checks[names.index(args.start_at):]
    if args.only:
        checks = [c for c in checks if c.name == args.only]
        if not checks: raise ValueError("Named check is absent from this lane")
    if args.list:
        for check in checks: print(check.name)
        return 0
    environment = {**os.environ, "SQLX_OFFLINE": "true", "FOSNIE_ROUTING_PYTHON": PYTHON, "FOSNIE_E2E": "0"}
    return run(checks, args.output, environment, merge=bool(args.only or args.start_at))

if __name__ == "__main__":
    try: sys.exit(main())
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print("Routing prerequisite failed: " + type(error).__name__ + ": " + str(error), file=sys.stderr)
        sys.exit(2)
