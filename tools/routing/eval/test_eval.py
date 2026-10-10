"""Evaluation infrastructure contracts; no live networking."""
from copy import deepcopy
import json
import os
import subprocess
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import corpus
import runner


class EvaluationContracts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.manifest, cls.cases = runner.load_dataset()

    def test_frozen_corpus_is_reproducible_and_split_by_template_family(self):
        manifest, cases = corpus.build()
        self.assertEqual(cases, self.cases)
        self.assertEqual(manifest["case_count"], 420)
        self.assertEqual(len({c["family_id"] for c in cases}), 70)
        self.assertEqual(len([c for c in cases if c["smoke"]]), 28)
        self.assertEqual({c["language"] for c in cases}, {"en", "ru"})
        for key in ("family_id", "split_group"):
            for family in {c[key] for c in cases}:
                self.assertEqual(len({c["split"] for c in cases if c[key] == family}), 1)

    def test_domain_validators_accept_truth_and_reject_wrong_sources_actions_and_types(self):
        for case in self.cases:
            expected = case["validator"]["expected"]
            content = corpus.encoded(expected) if expected is not None else None
            self.assertEqual(runner.validate_answer(case, content), "unknown" if expected is None else "pass")
            if expected is not None:
                self.assertEqual(runner.validate_answer(case, '{"incorrect":true}'), "fail")
                self.assertEqual(runner.validate_answer(case, content, [{"name": "send_message"}]), "fail")
        case = next(c for c in self.cases if c["category"] == "weather")
        wrong = deepcopy(case["validator"]["expected"]);wrong["source_id"] = "invented"
        self.assertEqual(runner.validate_answer(case, corpus.encoded(wrong)), "fail")
        self.assertFalse(runner.exact_json({"n": True}, {"n": 1}))
        self.assertFalse(runner.exact_json({"n": 1, "extra": 0}, {"n": 1}))

    def test_live_sampling_covers_categories_before_repeated_parameters(self):
        chosen = runner.stratified_select(self.cases, 28)
        self.assertEqual(len(chosen), 28)
        self.assertEqual(len({c["category"] for c in chosen}), 14)
        self.assertEqual({c["language"] for c in chosen}, {"en", "ru"})
        self.assertTrue(all(c["objective"] == "economy" for c in chosen))

    def test_dataset_hash_and_split_leakage_are_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            manifest = deepcopy(self.manifest)
            payload = (corpus.ROOT / "fixtures/cases.jsonl").read_bytes()
            (root / "cases.jsonl").write_bytes(payload + b" ")
            (root / "manifest.json").write_text(corpus.encoded(manifest), encoding="utf-8")
            with self.assertRaises(ValueError):
                runner.load_dataset(root)
            changed = deepcopy(self.cases);changed[0]["split"] = "calibration"
            payload = "".join(corpus.encoded(c) + "\n" for c in changed).encode()
            manifest["cases_sha256"] = runner.hashlib.sha256(payload).hexdigest()
            (root / "cases.jsonl").write_bytes(payload)
            (root / "manifest.json").write_text(corpus.encoded(manifest), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "leakage"):
                runner.load_dataset(root)

    def config(self):
        return {"schema_version": 1, "base_url": "https://synthetic.fosnie.test", "auth_env": "FOSNIE_EVAL_TEST_TOKEN",
                "total_budget_microusd": 350, "request_budget_microusd": 100, "max_cases": 2,
                "max_duration_ms": 1000, "max_output_tokens": 256,
                "policies": {s: {"id": p["id"], "revision": 1} for s, p in self.manifest["policies"].items()},
                "economy_provider_id": corpus.IDS["economy_local"], "single_provider_id": corpus.IDS["advanced_local"],
                "default_policy_id": self.manifest["policies"]["rules"]["id"]}

    def test_live_dry_mode_sends_zero_network_requests(self):
        with patch("urllib.request.build_opener", side_effect=AssertionError("no networking")):
            records, details = runner.live(self.manifest, self.cases, self.config(), execute=False)
        self.assertEqual(records, [])
        self.assertEqual(details["network_calls"], 0)
        self.assertEqual(details["live_quality"], "not_run")

    def test_live_configuration_rejects_credential_urls_redirects_and_missing_caps(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "live.json"
            for update in ({"base_url": "https://secret@host.test"}, {"base_url": "http://remote.test"},
                           {"request_budget_microusd": 0}, {"total_budget_microusd": True}, {"max_cases": 501},
                           {"schema_version": True}, {"auth_env": None}):
                value = self.config();value.update(update);path.write_text(corpus.encoded(value), encoding="utf-8")
                with self.assertRaises(ValueError):
                    runner.live_config(path)
            value = self.config();path.write_text(corpus.encoded(value), encoding="utf-8")
            self.assertEqual(runner.live_config(path), value)
        with self.assertRaises(ValueError):
            runner.NoRedirect().redirect_request(None, None, 302, "redirect", {}, "https://bad.test")

    def test_live_total_cap_keeps_failed_spend_and_never_logs_credentials_or_content(self):
        calls = []
        fixture = self
        class Response:
            def __init__(self, value):self.value = value
            def __enter__(self):return self
            def __exit__(self, *args):return False
            def read(self, size):return corpus.encoded(self.value).encode()
        class Opener:
            def open(self, req, timeout):
                path = runner.urllib.parse.urlsplit(req.full_url).path
                if path.endswith("/management"):return Response({"deployment_manage": True})
                if path.endswith("/policies"):
                    return Response([{"id": p["id"], "revision": 1, "scope": "deployment", "is_default": s == "rules", "policy": p}
                                     for s, p in fixture.manifest["policies"].items()])
                if path.endswith("/catalog"):
                    return Response([{"id": p["provider_id"], "enabled": True, "profile": p} for p in fixture.manifest["profiles"]])
                body = json.loads(req.data);calls.append(body)
                return Response({"ok": False, "content": "response-must-not-be-saved"})
        with patch.dict(os.environ, {"FOSNIE_EVAL_TEST_TOKEN": "synthetic-secret-must-not-be-saved"}), patch("urllib.request.build_opener", return_value=Opener()):
            records, details = runner.live(self.manifest, self.cases, self.config(), execute=True)
        self.assertEqual(len(calls), 3)
        self.assertEqual(sum(c["max_cost_microusd"] for c in calls), 300)
        self.assertEqual(details["reserved_cost_upper_microusd"], 300)
        self.assertTrue(details["budget_stop"])
        self.assertTrue(all(r["validator_status"] == "unknown" for r in records))
        self.assertNotIn("synthetic-secret", corpus.encoded(records))
        self.assertNotIn("response-must-not-be-saved", corpus.encoded(records))

    def test_unknown_costs_and_evaluator_costs_are_separate(self):
        rows = [{"validator_status": "pass", "strategy": "hybrid", "category": "recipe", "language": "en", "split": "test", "objective": "economy",
                 "routing_contract_status": "pass", "estimated_cost_microusd": cost, "classifier_calls": 1, "fallback_count": 0,
                 "under_routing": False, "advanced_for_noncritical_task": False, "evaluator_cost_microusd": 999} for cost in (0, None, 10)]
        aggregate = runner.summary(rows)["strategy:hybrid"]
        self.assertIsNone(aggregate["estimated_cost_microusd"])
        self.assertIsNone(aggregate["actual_cost_microusd"])
        self.assertEqual(aggregate["known_estimated_subtotal_microusd"], 10)
        self.assertEqual(aggregate["unknown_cost_rows"], 1)
        self.assertEqual(aggregate["evaluator_cost_microusd"], 2997)

    def test_production_replay_wire_rejects_unknown_fields_and_oversized_lines(self):
        binary = runner.CORE / "backend/target/debug/examples" / ("routing_replay.exe" if os.name == "nt" else "routing_replay")
        self.assertTrue(binary.is_file(), "build the production replay adapter before running this contract suite")
        value = runner.replay_input(self.manifest, self.cases[0], "rules")
        value["untrusted_extra"] = "SYNTHETIC_UNTRUSTED_MARKER"
        for payload in (corpus.encoded(value), '"' + "x" * 65537 + '"'):
            result = subprocess.run([str(binary)], input=payload + "\n", text=True, encoding="utf-8", capture_output=True, timeout=5)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn("SYNTHETIC_UNTRUSTED_MARKER", result.stderr)
            self.assertEqual(result.stdout, "")

    def test_report_records_source_hashes_without_requiring_git(self):
        with patch("subprocess.run", side_effect=FileNotFoundError("git unavailable")):
            versions = runner.source_versions()
        self.assertIsNone(versions["core_head"])
        self.assertTrue(all(len(digest) == 64 for digest in versions["source_sha256"].values()))


if __name__ == "__main__":
    unittest.main()
