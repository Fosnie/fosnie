---
kind: added
bump: minor
---

# Reaching the systems a practice actually runs on

## changelog

Added a rule for tools that reach outside during a telephone call, so nothing waits on an approval nobody can give while a caller listens, and outward notifications that post a line into Slack, Teams or any address that accepts a message when a line takes something.

## site

A telephone line can now reach the systems a practice really uses, and tell people about it where they actually look. Tools that change something outside are decided in advance rather than mid-call: allowed ones run under a hard time limit, and the rest make the agent offer to take a message instead of leaving the caller in silence. When a line takes a message or arranges an appointment, a line can be posted into your chat channel saying who rang and what about.

## detail

Everything a line did so far landed inside this deployment. A practice keeps its diary and its conversations elsewhere, so this is the part that reaches them.

**Some of it already worked, and one thing quietly ruined it.** A line's agent can be given tools that reach an outside system, and those tools were already offered on a call. But a tool that changes something is held for a person to approve, which is right in front of a screen and quite wrong on a telephone: nobody is watching an approval in the middle of somebody else's call, so the caller would have sat listening to nothing for as long as the approval was allowed to wait.

**So on a call, nothing waits for a person.** The decision is made beforehand instead. A server or a tool definition is either marked as usable during a call or it is not. One that is not is refused, and the agent is told to say it cannot do that on the telephone and to offer to take a message, so the caller gets an answer rather than a silence. One that is marked runs, and the fact that a caller caused a change outside the deployment is written into the record every time.

**Refused by default, and marked per server.** A caller is an anonymous member of the public, and what they can reach is whatever the line's agent holds. Turning that into permission to write into a corporate system is a decision an operator makes deliberately, once, in front of a plain description of what it means. Reading is different: a tool that only looks something up runs on a call as it does anywhere else.

**And a hard limit on how long a caller waits.** Any tool call on a call has a ceiling, eight seconds by default and settable. Past it the caller is told the same thing as a refusal, and the conversation carries on. What is not tolerable is silence on a live line while something slow is queried.

**Telling somebody outside.** A message taken at four in the afternoon is no use if it is seen tomorrow, so a line can now post into a chat channel: who rang, what it is about, and a way back into here for the rest. Slack, Teams, or any address that accepts a posted message. Several destinations are allowed, each choosing which of the four events it takes: a message taken, an appointment booked, moved or cancelled.

**What leaves is exactly what the internal announcement says.** Who rang and what about, capped in length, and never a word of what was said. The people in a channel are not necessarily the people entitled to read what a caller dictated, and whoever is can read it where it is kept.

**Nothing is sent until an administrator switches outward notifications on**, like every other connector here, and every attempt passes the same gate and is recorded. The address is treated as a credential, because it is one: anybody holding it can post into that channel. It is stored encrypted, never shown again, and a saved destination shows only the host it points at. There is a test button, because an address pasted wrong otherwise fails quietly and the first anybody hears of it is a client who was never rung back.

**A notice is never lost to somebody else's outage.** Deciding to tell a channel and actually posting to it are separate: the call never waits on a chat service, and a service that is down is retried rather than forgotten.

One thing worth saying plainly: this is the road to an outside diary rather than the diary itself. A practice that keeps its calendar in a corporate system reaches it the same way it reaches anything else, through a connected server whose tools the agent may use, and this is what makes that usable on a live call. Booking straight into an outside calendar as though it were the built-in one is still to come.
