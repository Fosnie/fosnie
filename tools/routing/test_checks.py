"""Acceptance queue must exercise current contracts, never consume model evals."""
import unittest
from checks import queue, validate_db

class CurrentRoutingQueue(unittest.TestCase):
    def test_vnext_covers_new_and_legacy_contracts_with_real_ee(self):
        checks=queue("vnext")
        by_name={c.name:c for c in checks}
        for name in ("protocol","build","core-isolation","unit","vnext-catalogue","integration",
                     "provider-failures","durable","stack","vnext-ml","vnext-frontend",
                     "acceptance-types","browser","ee-build","acceptance-ee","frontend-ee"):
            self.assertIn(name,by_name)
        self.assertTrue(by_name["vnext-catalogue"].db)
        self.assertTrue(by_name["acceptance-ee"].db and by_name["acceptance-ee"].enterprise)
        self.assertEqual(len(checks),len(by_name))
        self.assertEqual([c.name for c in queue("vnext",True)],[c.name for c in checks])

    def test_consolidation_never_dispatches_live_or_historical_evaluations(self):
        checks=queue("vnext")
        self.assertFalse(any(c.name.startswith("eval-") or c.name=="live-not-run" for c in checks))
        ml=next(c for c in checks if c.name=="vnext-ml")
        for name in ("variant_generation","native_replay","provider_failure_details","inference_failure"):
            self.assertIn("tests/test_"+name+".py",ml.args)
        self.assertFalse(any("routing_source_qa" in arg or "routing_query" in arg or "routing_corpus" in arg for arg in ml.args))
        frontend=next(c for c in checks if c.name=="vnext-frontend")
        self.assertIn("src/api/routingCatalogue.test.ts",frontend.args)
        self.assertIn("src/api/routingVNext.test.ts",frontend.args)

    def test_database_guard_refuses_production_and_missing_prerequisites(self):
        for value in (None,"postgres://example.com/routing_private","postgres://127.0.0.1/production"):
            with self.assertRaises(ValueError):validate_db(value)
        validate_db("postgres://127.0.0.1/routing_vnext_fixture")

if __name__=="__main__":unittest.main()
