"""Predeclared native timing/availability rules, independent of model quality."""
from collections import Counter
import threading
import time

class RequestTiming:
    def __init__(self, *, clock=time.monotonic, sleep=time.sleep, spacing=10.0, core_seconds=30.0, judge_seconds=12.0, failure_limit=3):
        self.clock=clock;self.sleep=sleep;self.spacing=spacing;self.core_seconds=core_seconds
        self.judge_seconds=judge_seconds;self.failure_limit=failure_limit
        self.last={};self.failures=Counter();self.blocked={};self.lock=threading.Lock()

    def ready(self, provider):
        with self.lock:
            if provider in self.blocked: raise TimeoutError('provider unavailable')
            delay=max(0,self.spacing-(self.clock()-self.last[provider])) if provider=='gemini-lite' and provider in self.last else 0
        if delay:self.sleep(delay)

    def dispatch(self, provider, purpose, started):
        self.ready(provider)
        now=self.clock();remaining=self.core_seconds-(now-started)-1.0
        limit=min(25.0,remaining,self.judge_seconds if purpose=='judge' else 25.0)
        if limit<1.0:raise TimeoutError('native request has no Core deadline room')
        with self.lock:self.last[provider]=now
        return limit

    def observe(self, provider, purpose, *, status=None, transport_failed=False):
        with self.lock:
            if status in (401,403,429):
                self.blocked[provider]='authorisation_or_quota';return
            key=(provider,purpose)
            if transport_failed or status is not None and status>=400:
                self.failures[key]+=1
                if self.failures[key]>=self.failure_limit:self.blocked[provider]='consecutive_transport_failures'
            else:self.failures[key]=0

    def snapshot(self):
        with self.lock:return dict(self.blocked)
