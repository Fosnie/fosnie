---
kind: added
bump: minor
---

# Switching the telephone on

## changelog

Added the telephone settings to the interface and a readiness check that asks the questions a call asks, including a real test request to the speech engine, so a line can be proved to work before anybody rings it.

## site

The telephone can now be set up and checked without touching a command line. A readiness check asks what a call asks, in the order it asks it, and tells you what to put right: whether something answers calls, whether the credential is stored, whether callers can reach you, and whether the line can actually speak.

## detail

The telephone feature was complete and unreachable. Everything that decides whether a deployment answers a call at all, what answers it, where callers reach it, and the credentials underneath, was set from a command line by whoever operates the instance. This is the part that makes it usable.

**The settings are in the interface now**, under the operator's own panel: what answers a call, the address callers reach you at, the ceiling on simultaneous calls, and for a line answered by your own telephone system, the address to listen on and the shared secret. Both credentials are write-only, as every secret here is: what comes back is whether one is stored, never a character of it. The guidance sits beside each field, including the lines to paste into your own telephone system's call routing.

**And a check that tells you whether a call will work.** Every way a telephone line can be misconfigured looks the same from outside: it does not work, and the person who finds out is a caller. So the check asks the questions a call asks, in the order a call asks them, and each answer says what to do about it:

- the telephone is switched on, and the accounts behind the lines can hold a voice conversation;
- something is named that can answer, and which of the two it is;
- for a carrier: the credential is stored and callers have a public address to reach;
- for your own telephone system: an address is being listened on **right now**, and a shared secret is stored;
- credentials can be stored safely at all;
- recognition is set to a rate a telephone line can be converted to;
- there is a number registered and switched on, and every line that is on has an agent and an account behind it.

**One of those is a real request rather than a setting read**, and it is the one that matters most. A deployment with no working speech engine answers a call, cannot say what the caller is speaking to, and ends the call. That is the correct thing to do and a terrible first impression, and it is entirely knowable in advance: the check sends a short test phrase to the configured engine and reports what came back. Configured and working are different facts, and only the second one takes a call.

**Two places, one answer.** The operator's panel shows it beside the settings it is about. The telephone screen shows it too, because the person registering a number is usually not the person who configured the carrier, and being told what is wrong is what stops a line being blamed for a deployment's fault. A readiness report is read by more people than can set the credentials it describes, so no part of it ever contains one.

The check makes a real request to your speech engine, so it runs when you ask for it and never on its own.
