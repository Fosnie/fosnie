---
kind: added
bump: minor
---

# Putting the caller through

## changelog

Added the ability for a telephone line to hand a caller to a person, with a written handover for whoever picks up, and for the agent to finish a call itself; no part of it makes an outbound connection to the telephone network.

## site

A line can now do the thing every receptionist does when they cannot help: put you through. Give a line a number and the agent can hand the call to it, having first told the caller what is happening and written down what they had already explained, so nobody is asked it all again. It can also end a call politely once everything is dealt with.

## detail

Until now a call could only end. The caller hung up, the network reported it was over, or the line went quiet long enough to be given up on. An agent that could not help had one thing to offer, which was to take a message, and no way to finish a call even when there was plainly nothing left to say.

Both are now possible, and the interesting part is what neither of them needs. Nothing here connects out to the telephone network. The network is already holding the call open and asks us what to do with it, so a transfer is something we are asked about rather than something we go and do: when the agent stops speaking, the network comes back for one more instruction and is told to ring somebody. That means no account credential for the network beyond the one already used to check that a request really came from it, no new outbound connector, and nothing at all added to what a deployment reaches out to. For a product whose default is that nothing leaves the building, that was worth finding.

**Where a call goes is set on the line, and the agent never chooses it.** It is a number an administrator puts against the line, and the only decision left to the agent is whether to use it. This is deliberate: if the number were something the agent worked out, it would be something a caller could talk it into working out, and a caller who picks the number is a caller who can have your deployment ring anybody, from your line, at your expense. A line with no number set does not offer the ability at all, rather than offering it and then apologising, because an agent that has told somebody it is connecting them has already said the wrong thing.

**The caller hears why before the line moves.** The agent decides during the reply it is in the middle of giving, so acting on that decision the moment it was made would cut the line part-way through "putting you through now". Instead the line waits until the network confirms it has actually played the words out, and only then hands the call over. That ordering is checked directly rather than assumed.

Whoever picks up gets a handover: what the caller wanted, in one line and in a short summary, written before the call moves and kept with the call afterwards. It is read in the same place as messages and enquiries, under the same rule about who may read it, so a delegated administrator sees that a call was put through and not what was said on it. The number presented to the person being rung is the line's own, because that is the number the deployment owns and is entitled to present; who actually rang is unverified and is in the written record where it belongs.

If nobody answers within about twenty-five seconds, the caller is told so and the call ends. That one sentence is the only thing in this feature spoken in the network's own voice rather than yours: by the time it is needed your side of the call has already ended and there is nothing of yours left to say it with. It is a fixed sentence and carries nothing about the call or the caller.

Finishing a call is the same machinery with nothing to write down. The agent says goodbye, the caller hears it, and the call ends: it is not a way to get rid of somebody still asking for something, and the instructions say so.

Two things worth knowing. A line that can put callers through costs one extra request to your deployment per call, because that is the request the network makes to ask what happens next; lines that cannot transfer are untouched and cost nothing extra. And a transfer is a handover rather than an introduction: the person picking up is connected to the caller directly, and reads the summary rather than being told it aloud.
