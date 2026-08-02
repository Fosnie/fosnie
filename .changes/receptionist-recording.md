---
kind: added
bump: minor
---

# Listening to a call

## changelog

Added call recording per line, kept as a two channel sound file you can play back from the call log, with the caller told the call is recorded before they say anything and a compulsory period after which the audio is deleted.

## site

A line can now keep the sound of its calls, so you can listen to what your receptionist and your client actually said. It is off everywhere until you switch it on, the caller is told before they speak, and you have to say how long recordings are kept: there is no keep for ever for somebody's voice.

## detail

Until now nothing here kept a second of sound. Speech was recognised as it arrived and the samples were discarded, which is why the notice told callers their words were written down rather than that the call was recorded. That is a good default and the wrong answer for a practice that needs to hear how a client was dealt with.

**Switched on per line, and off until you do it.** A practice can record the client line and not the internal one. The recording is of the whole conversation from the moment the call is answered.

**Turning it on changes what your line says, and that is the point.** A recording line adds one sentence to what every caller hears, before they have said anything: this call is recorded. That sentence is the difference between a recording and a covert one, so the switch and the wording are one change rather than two: the interface shows you the exact words your callers will hear as you turn it on, and there is no way to configure a line that records without saying so.

**Two channels, so you can hear who spoke.** The caller on one side, your line on the other, laid out on the clock rather than stitched together: a pause is silence in the right place, and both of them talking at once sounds like both of them talking at once. That is what makes a recording of a conversation worth listening to rather than merely having.

**Play it from the call log**, where the length and the size are shown beside it. Only the account whose line took the call can listen, or an administrator of this deployment, and **every listen is recorded in the audit trail**: hearing a member of the public speak is an act rather than a page view.

**A period is compulsory.** Every other retention setting here treats nought as keep indefinitely, which is right for a line of text and wrong for a voice. A line cannot be set to record without saying for how long, and after that the audio is deleted from the disk by the nightly sweep. Deleting what was said on a call deletes its recording too, because somebody asking for their information to be removed does not mean the text only. And a recording whose call has gone, through an erasure or a retention period, is swept up as well: it would otherwise be a voice recording that nothing in the product could see, keep or delete.

**The record of what your lines hold now says so.** The line that used to state that no audio is kept at any point is worked out from your settings rather than declared, so on a deployment that records it says what is kept and for how long, per line. That is the single most important line in that record and it is now unable to be wrong.

Two things worth knowing. About a megabyte a minute, so a busy line is a few gigabytes a year, which is why the period is required rather than suggested. And a recording ends where your side of the call does: once a caller has been put through to a person, the conversation is between them and the telephone network and never reaches this deployment.
