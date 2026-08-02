### Added

- Added the ability to take voice calls on telephone numbers, each answered by an agent you choose on behalf of an account you choose, registered and switched on in the interface, with a call log linking to the transcript of every call, and with speech recognition, the reply and the conversation all running on your own infrastructure.
- Added separate turn-taking settings for calls arriving on a telephone line, timing for every stage of a spoken reply, and a measurement of what the caller actually heard rather than what was sent.
- Added the ability for the agent answering a telephone line to write down what a caller wanted and pass it on, with a ready-made Receptionist agent, an optional announcement into a team chat, and a rule that whoever may wire a line still cannot read what callers said on it.
- Added the ability for a telephone line to hand a caller to a person, with a written handover for whoever picks up, and for the agent to finish a call itself; no part of it makes an outbound connection to the telephone network.
- Added a list of names a telephone line checks callers against before offering them anything, enforced so that no caller is put through to a person until they have been checked and found clear, with the result recorded against the call and never disclosed to the caller.
- Added a diary with real opening hours and a real time zone, so a telephone line can offer times, book one, move it and cancel it, with two callers unable to take the same slot and a caller ringing back identified by two independent things before anything changes.
- Added a spoken notice every caller hears before anything they say is acted on, per line retention for conversations and call records, deletion of a single call's conversation on request, and a record of what the lines do with what they are told, assembled from the settings themselves.
- Added a rule for tools that reach outside during a telephone call, so nothing waits on an approval nobody can give while a caller listens, and outward notifications that post a line into Slack, Teams or any address that accepts a message when a line takes something.
- Added a second way for a line to be answered: a telephone system on the practice's own network hands the audio straight to the deployment, so a call reaches nobody else at all.
- Added the telephone settings to the interface and a readiness check that asks the questions a call asks, including a real test request to the speech engine, so a line can be proved to work before anybody rings it.
- Added call recording per line, kept as a two channel sound file you can play back from the call log, with the caller told the call is recorded before they say anything and a compulsory period after which the audio is deleted.

### Changed

- Live voice can now be carried over something other than a browser tab: the conversation, its turn taking and its interruptions are no longer tied to one kind of connection, and the narrowband audio handling a telephone line needs is in place ready for it.

### Fixed

- Fixed wide tables in the admin console giving the whole page a horizontal scrollbar: a table with more columns than the page is wide now scrolls inside its own box, leaving the headings and prose beside it where they were.

Full notes: https://docs.fosnie.dev/changelog/v0.6.0
