import unittest
from request_timing import RequestTiming

class Clock:
    def __init__(self):self.now=100.0;self.waits=[]
    def clock(self):return self.now
    def sleep(self,seconds):self.waits.append(seconds);self.now+=seconds

class RequestTimingTests(unittest.TestCase):
    def setUp(self):
        self.time=Clock();self.timing=RequestTiming(clock=self.time.clock,sleep=self.time.sleep)

    def test_initial_spacing_precedes_core_and_second_call_fits_absolute_deadline(self):
        self.timing.last['gemini-lite']=95
        self.timing.ready('gemini-lite')
        started=self.time.now
        judge=self.timing.dispatch('gemini-lite','judge',started)
        self.assertEqual(judge,12)
        self.time.now+=4
        primary=self.timing.dispatch('gemini-lite','primary',started)
        self.assertEqual(primary,19)
        self.assertLessEqual(self.time.now+primary,started+29)
        self.assertEqual(self.time.waits,[5,6])

    def test_late_or_spaced_request_is_refused_before_upstream(self):
        started=self.time.now;self.timing.last['gemini-lite']=self.time.now+25
        with self.assertRaises(TimeoutError):self.timing.dispatch('gemini-lite','primary',started)

    def test_one_timeout_is_retained_without_disabling_other_provider_or_new_case(self):
        self.timing.observe('gemini-lite','judge',transport_failed=True)
        self.assertFalse(self.timing.snapshot())
        self.timing.observe('openai','primary',status=200)
        self.assertFalse(self.timing.snapshot())
        self.timing.observe('gemini-lite','judge',status=200)
        self.assertEqual(self.timing.failures[('gemini-lite','judge')],0)

    def test_three_failures_stop_fixed_pool_and_auth_or_quota_stops_immediately(self):
        for _ in range(3):self.timing.observe('gemini-lite','judge',transport_failed=True)
        self.assertEqual(self.timing.snapshot(),{'gemini-lite':'consecutive_transport_failures'})
        for status in (401,403,429):
            timing=RequestTiming();timing.observe('openai','primary',status=status)
            self.assertEqual(timing.snapshot(),{'openai':'authorisation_or_quota'})

    def test_openai_does_not_inherit_gemini_rate_spacing(self):
        self.timing.last['openai']=100
        self.timing.ready('openai');self.assertFalse(self.time.waits)

if __name__=='__main__':unittest.main()
