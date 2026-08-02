---
kind: added
bump: minor
---

# Tuning a telephone line, and measuring it

## changelog

Added separate turn-taking settings for calls arriving on a telephone line, timing for every stage of a spoken reply, and a measurement of what the caller actually heard rather than what was sent.

## site

A telephone line can now be tuned separately from the browser: it waits less before deciding a caller has finished, yields sooner when talked over, and judges pauses rather than counting them. Every stage of a spoken reply is timed, including the one that matters most, which is when the caller began hearing it.

## detail

A telephone is not a quieter browser tab. It carries a narrow band of frequencies and a constant hiss, its echo cancellation belongs to the phone network rather than to us, it has no button to hold, and above all it shows the caller nothing: a pause with no reply reads as a dropped call, and callers start saying "hello?". The settings that govern turn-taking were chosen for a browser, and until now there was one set of them for both.

There are now two. Any of these settings can be given a separate value for calls, and one that is not simply follows the shared value, so a change made for the browser still reaches a line unless the line has been given its own. Out of the box a call waits 900 milliseconds after speech stops rather than 1500, needs slightly more speech before treating a noise as a sentence, and yields after 240 milliseconds of being talked over rather than 320, because what a caller hears is that delay plus everything still queued between here and their handset.

Calls also judge pauses semantically by default, which a browser does not. That is the difference between waiting out a mid-thought pause and cutting somebody off, and it matters far more on a telephone where there is nothing on screen to explain the wait. It needs the turn-detection service configured; without it, calls fall back to the timer.

Which brings us to a fault worth describing plainly, because we found it while building this. Semantic turn detection ends a turn when the detector agrees the speaker has finished. There is no answer a detector can give that means "I have stopped working", so a detector that stopped answering would hold every turn indefinitely. In a browser that is a stalled screen. On a telephone it is a call that stays open, silent and billable, until the caller gives up. Two independent safeguards now prevent it: a service that fails to answer is not consulted again for the rest of that call, and no detector may hold a single turn beyond four times the configured pause however it answers. Either one alone is enough, and both are checked by a test that drives a real call against a detector that refuses everything.

On measurement, the headline number has changed meaning and it is worth saying so. It used to be recorded when reply audio was handed to the connection. On a telephone that is early: the audio is still to be paced onto the line, sent, and played out of whatever the phone network is holding. So a call now also reports when the caller **began** hearing the reply and when they finished, by asking the network to confirm playback and timing the answer. The gap between the two is the connection's own delay, measured rather than assumed, and the published target for a reply starting within eight tenths of a second now applies to the figure a caller would recognise. Every intermediate stage is timed too: how long after speech stopped the turn ended, how long the detector took, how long until the first word came back from the model, how long until the first sentence was ready to speak.

Two consequences of that we would rather state than have anybody discover. The clock now starts when speech recognition settles rather than a step later, so it includes announcing the transcript and recording it, both of which a speaker waits through: the figure reads slightly higher than before, on purpose. And the reply-latency series has been renamed to report in seconds like every other timing in the system, so a dashboard imported from an earlier release needs the new one.

Two smaller repairs came with it. Timing metrics were being published in a form that percentile queries cannot read, which meant several shipped dashboard panels were blank and two alerts could never have fired; that is fixed for every timing metric, not only the new ones. And the loudness thresholds that decide what counts as speech, and what counts as talking over a reply, are now adjustable and published as a distribution. They keep their existing values on a telephone deliberately: they were chosen for a microphone, we have not measured where speech sits relative to a phone line's background noise, and we would rather ship the instrument to find out than a number we invented.
