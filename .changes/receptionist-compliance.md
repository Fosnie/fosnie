---
kind: added
bump: minor
---

# Telling the caller, and throwing things away

## changelog

Added a spoken notice every caller hears before anything they say is acted on, per line retention for conversations and call records, deletion of a single call's conversation on request, and a record of what the lines do with what they are told, assembled from the settings themselves.

## site

A line now tells every caller what they are speaking to and what becomes of what they say, before it listens to a word. It cannot be talked over, it is written down beside the call, and a line that cannot say it does not take the call. Alongside it: how long each line keeps what was said and the record of the call, both set by you and both off until you set them, and a page stating in one place what your lines hold and for how long.

## detail

Until now a line answered in silence. The caller spoke first, and was recognised, checked and possibly given an appointment without ever being told they were speaking to a machine or that their words were being written down. That is the gap this closes, and it also happens to be why the line felt broken: nobody expects to be answered by silence.

**Every caller is told, first, and it cannot be talked over.** The line speaks its greeting and its notice before it listens to anything. A caller who talks across it is neither cut off nor answered underneath it: their words go nowhere at all until the notice has finished, and only then does the conversation start. The standard wording tells the caller they are speaking to an automated assistant, that what they say is written down so their enquiry can be dealt with and that a member of staff may read it, and that they can ask for a person instead. You can write your own for a line, and the interface shows you the exact sentence a caller will hear as you type it.

**It says written down, not recorded, because that is what happens.** No audio of a call is kept anywhere: speech is recognised as it arrives and the sound is discarded. A notice claiming a recording that does not exist would be as wrong as one hiding a transcript that does.

**A line that cannot say it does not take the call.** If synthesis is unavailable, the call ends rather than continuing in silence, and it is recorded as having ended for that reason so it is plain in the log what happened. Little is lost in practice, because a line that cannot speak could not have answered anything anyway, and nothing is lost in the case that matters: nobody is listened to who was not told. What each caller was actually told, in the words they heard, is kept beside their call, because the question a complaint asks is what was said on that call rather than what the line says today.

**Two retention periods a line, both off until you set them.** One decides how long what was said is kept: after it, the conversation is deleted and the call stays in the log, marked as tidied away, with who rang, when, for how long and how it ended. The other decides how long that record itself is kept. Nought means indefinitely, which is what every line does until somebody decides otherwise, so nothing starts deleting on its own.

**What a caller left behind is never deleted by either of them.** Messages and enquiries, appointments, and your list of names to check callers against are your own records rather than a by-product of a call. A retention period on a telephone line has no business deleting an appointment somebody is expecting to be kept, so it does not, and each of those simply lets go of its reference to the call when the call goes.

**One call's conversation can be thrown away on request**, from the call log, for the moment somebody asks for their information to be removed and will not wait for the nightly sweep. The account whose line took the call may do it, and so may an administrator of the platform; nobody else is told the call exists.

**And there is now something to hand a data protection officer.** A record of what these lines do with what they are told: the exact words each line says to callers, whether a person can be reached and where, how long each part is kept, how many calls each line has taken and how many of those were told what they were speaking to, what is held and how much of it, and two things no settings page implies: that no audio is kept at any point, and that nothing leaves the deployment except the call itself, which is the telephone network's. It is read from the settings as they stand rather than written down once, so it cannot quietly stop being true, and it copies out as plain text for an assessment.

Two things worth knowing. The notice is speech, so it costs the seconds it takes to say: the standard one is four short sentences, and the preview is there so you can see what you are lengthening. And retention is per line rather than per caller: a request to remove one person's information is served by deleting that call's conversation, which is why that can be done by hand.
