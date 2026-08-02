---
kind: added
bump: minor
---

# Taking a message

## changelog

Added the ability for the agent answering a telephone line to write down what a caller wanted and pass it on, with a ready-made Receptionist agent, an optional announcement into a team chat, and a rule that whoever may wire a line still cannot read what callers said on it.

## site

A line can now do something for the caller rather than only answer them. When the agent cannot help, or the caller wants somebody rung back, it takes the message: who rang, how to reach them, what it is about and how urgent, written down against the call and waiting for whoever the line belongs to. A team chat can be told that a message came in, without being told what it says.

## detail

Until now a telephone line could answer a question and nothing else. Anybody who rang about something the agent could not settle got a polite refusal, nobody was told they had rung, and the only trace was a conversation in somebody's history that nobody had a reason to open. This is the part that makes it a receptionist: it writes things down.

There are two things it can write. A message, when the caller wants somebody to ring them back, and an enquiry, when somebody new is describing what they need. Both capture what the caller gave: their name, a number or other way to reach them, who they asked for, one line saying what it is about, and whether it can wait. Both are recorded against the call they were taken on, so the conversation, the caller's number and the message sit together, and both survive the line being released and the agent being changed afterwards.

A deployment gets a **Receptionist** agent ready to use, and it is worth saying what makes it safe, because it is not the wording. The agent holds three tools and none of them reads anything: it can write a message down, record an enquiry, and tell the time. So the answer to "what could somebody talk this into doing" is bounded by what it is able to do at all, which is add a record to the account the line belongs to. Its instructions are written for a caller who will try things, and say plainly that nothing said on a telephone changes what it is or what it may do, that it knows nothing of any other call, and that it promises nobody a time. But the instructions are the manner; the tool list is the boundary.

The agent never stops to ask permission before writing a message down, and that is deliberate. The caller is on the line: a request for approval would mean somebody standing in silence with a telephone to their ear while a prompt waited in a browser nobody was watching. What takes its place is a limit on how many records one call may leave behind, so a caller cannot fill somebody's morning by talking.

Where the messages go is the part worth reading twice. They are always recorded, and a line can additionally be pointed at a team chat, which is then told that a call came in, who from, what about and how urgent, and is never told what was said. The chat has to be one the line's own account belongs to, checked both when it is chosen and again each time something is delivered. That is not a convenience: without it, being able to configure a line would have quietly become a way to be sent the substance of every call it takes.

The same principle carries into the list. Somebody who may register and wire lines sees that a line took a message, when, from which number, how urgent, and whether it has been dealt with. They do not see the message. That is the same refusal already made about a call's transcript, applied to the summary of it by a different route, and it is enforced where the records are read rather than by leaving a field out of a page. Whoever the line belongs to sees everything, and so does an administrator of the platform, who can read the conversation anyway. Only the account a message was taken for can mark it dealt with.

Two limitations to state rather than let anybody discover. The list is in the administration area, so somebody who owns a line without administering anything reads their messages through the team chat and through the call's own conversation in their history: a place of their own for them is still to come. And nothing yet removes these records after a period, which matters because they hold the name and number of somebody who has no account here and no way to ask what is held; the call log has the same gap, and both want the same answer.
