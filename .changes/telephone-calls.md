---
kind: added
bump: minor
roadmap_id: voice-receptionist
---

# Answering the telephone

## changelog

Added the ability to take voice calls on telephone numbers, each answered by an agent you choose on behalf of an account you choose, registered and switched on in the interface, with a call log linking to the transcript of every call, and with speech recognition, the reply and the conversation all running on your own infrastructure.

## site

Fosnie can now answer the telephone. A caller rings one of your numbers, talks to the agent you put behind it, and is answered in speech: the recognition, the reply and the voice all run on the same infrastructure as the rest of your deployment, so nothing about the conversation leaves it except the call itself, which the telephone network carries either way. Numbers are registered and switched on in the interface, and every call appears in a log that links to the conversation it produced.

## detail

Live voice has worked in the browser for some time. It now works on a telephone line, which is the same conversation reached a different way: the same turn taking, the same knowledge, the same interruption handling, and the same record of what was said afterwards.

What this means in practice is that the running cost of an answered call is the telephone network's charge and nothing else. The recognition, the language model and the voice are already part of a Fosnie deployment, so a call adds no charge for any of them. For a line taking a few hundred calls a month, that is a few pounds of telephony.

Setting it up takes a number from a telephony provider and the credential that provider issues, both named once by whoever operates the deployment. Everything after that happens in the interface, in a Telephone section with a permission of its own, so answering the telephone can be delegated without handing anybody the rest of the platform's settings. Register as many numbers as you hold, and give each one two things: the account its calls run as, and the agent that answers them.

The agent is the important one. A caller has no account and is not signed in, so what they can reach is exactly what that agent can reach: the tools it has been given and the Libraries it has been attached to, and nothing else on the deployment. An agent with no tools and one Library of opening hours and services is a receptionist that cannot be talked into anything more, which is why each line is listed with the number of tools its agent holds. Register a line to an ordinary account rather than an administrator's, because an administrator can read every Library and the line would inherit that.

A new line arrives switched off, so nothing answers in the moments between registering a number and checking it. Switching a line off is the reversible way to stop it answering; releasing it removes it for good, and the calls it took stay in the log. The whole telephone surface stays dormant until a provider is named, so a deployment that does not want a telephone has no telephone surface at all: the addresses a provider would call are not merely refused, they are absent. Every request from the provider must carry their signature or it is rejected before anything reads it, the number of calls running at once is capped, and repeated calls from the same number are throttled. A call to a number you have not registered and a call to one you have switched off are turned away identically, so nobody outside can learn which of your numbers are live. Both are recorded with their reason in the audit trail, as is every change anybody makes to a line.

Each answered call becomes an ordinary conversation belonging to the account the line runs as, titled by what it was about and marked in the conversation list as having arrived by telephone. The call log lists what happened on each line: when the call started, who rang, how long it lasted, how it ended, and a link to the transcript. That link is the only way to read the words, and it opens the conversation under the permission that already governs reading that account's conversations. So somebody who may register a line, but who neither owns the account nor administers the platform, can see that a call happened and not a word of what was said. That is deliberate.

Two things are worth knowing before switching a line on. The public address of your deployment has to be configured to match what the provider has, exactly, because it forms part of what their signature covers; if it does not match, the line will ring and never answer, and the audit trail will say so. And the reply needs a streaming voice engine configured, because a telephone line carries raw audio rather than an audio file: without one, the line refuses calls rather than answering them silently.

Interruption works: a caller who talks over the reply is heard, and what they were talking over stops. A greeting can be noted against a line for your own reference, though it is not yet spoken. Keypad digits, transferring a call to a person, opening hours and out of hours routing, and booking into a diary are not here yet.
