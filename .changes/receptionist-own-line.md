---
kind: added
bump: minor
---

# A telephone line that runs on your own system

## changelog

Added a second way for a line to be answered: a telephone system on the practice's own network hands the audio straight to the deployment, so a call reaches nobody else at all.

## site

Until now a call was carried by a telephone company, which meant the caller's voice passed through them. A line can now be answered by your own telephone system instead: the audio goes from your equipment to your deployment over your own network, and no third party is involved in any part of the conversation.

## detail

Everything else about this product keeps a firm's material inside its own perimeter. The telephone did not: the documents, the model and the transcripts stayed put, and the caller's actual voice went to a telephone company in another country. This closes that.

**Your own equipment carries the call.** A telephone system on your network, of the kind a practice with a switchboard already runs, hands the audio straight to the deployment and takes the reply back the same way. Nothing about the call reaches anybody else: not the audio, not the words, not who rang.

**The line behaves exactly as it did.** The same notice at the top of the call, spoken before anything the caller says is listened to. The same conversation, the same messages taken, the same appointments, the same screening, the same retention. It is the transport underneath that changed, which is what the design was built for: everything above it did not have to be written twice.

**Putting a caller through still works**, and works the way the rest of it does. When our side of the call finishes, your system asks whether anybody is to be rung and dials them if so. The number is decided here and never by the caller.

**A port that is not an open door.** Your telephone system asks the deployment what to do with an incoming call and is given a one-off identifier, good for thirty seconds and usable exactly once. A connection presenting anything else is closed without a word. The two questions your system asks carry a shared secret and are only accepted from your own network, which is what stands in for the signature a telephone company puts on its requests. Nothing is listened for at all until an operator sets an address to listen on, so a deployment answering through a telephone company opens no port of its own.

**Both ways of answering can run side by side.** A line says which one answers it, so a practice can keep a public number with a telephone company and answer an internal one on its own system, with the same agent and the same records behind both.

Setting it up needs three things from whoever operates the deployment: an address to listen on, a shared secret, and half a dozen lines in the telephone system's own call routing. Those lines are shown in the telephone settings, ready to paste and edit.
