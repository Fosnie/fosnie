---
kind: added
bump: minor
roadmap_id: voice-live-retrieval
---

# Live voice searches your Libraries while you speak

## changelog

Added speculative library search to live voice: the knowledge-base search now starts from the partial transcript while the speaker is still talking, so a grounded reply begins sooner.

## site

Live voice now starts searching your Libraries while you are still speaking, rather than waiting until you stop. The reply comes back grounded in your own sources, and noticeably sooner.

## detail

In a spoken conversation, the search of your Libraries used to happen entirely after you stopped talking. It sat squarely inside the gap before the assistant replied, and on a large corpus that search takes seconds. Meanwhile the natural pauses in your own sentence were dead time.

Live voice now uses them. As you speak, it watches the transcript settle and starts searching on the part that has stopped changing. When you pause in a way that suggests you are finishing, it makes one more search on the whole sentence, which is usually the one that counts. By the time you actually stop, the passages are already in hand.

Speaking is not a committed question, so nothing is trusted blindly. When your turn ends, the query that was searched for is compared with what you actually said. If you simply finished the sentence, or re-worded the same question, the result is used and the reply skips its own search entirely. If you changed your mind mid-sentence, everything speculative is discarded and the turn searches exactly as it always did, with no difference in the answer you get. A search still running is dropped the moment a newer one starts, and again if you interrupt the assistant.

Because a speculative search runs a deliberately light profile on a sentence that was still being spoken, it may not cover the whole question. A turn that uses one is therefore always given the follow-up search tool, so the model can fill in anything missed while it composes the answer.

Access control is unchanged and inherited rather than reimplemented: a speculative search resolves the same Library scope, with the same per-document restrictions, that the turn itself would, and refuses to run at all if that scope cannot be resolved cleanly. Every speculative search is audited like any other retrieval.

The feature is on by default and needs a streaming speech-to-text engine to have anything to work from; with batch transcription there are no partial transcripts and it simply never fires. Administrators can tune how eagerly it speculates, how many searches a single sentence may trigger, and how closely a search must match what was said before its result is used.
