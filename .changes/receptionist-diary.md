---
kind: added
bump: minor
---

# Arranging a time

## changelog

Added a diary with real opening hours and a real time zone, so a telephone line can offer times, book one, move it and cancel it, with two callers unable to take the same slot and a caller ringing back identified by two independent things before anything changes.

## site

A line can now do the thing most people ring for. Set your opening hours, how long an appointment is and which time zone you keep, and the agent offers free times in your own local hours, books one, and gives the caller a reference to quote if they need to change it. Two callers cannot take the same slot, and nothing is arranged for anybody who has not passed your own checks.

## detail

Everything so far let a line answer, explain, take a message and hand the caller to a person. This is the part that arranges something.

Set the diary once: which time zone you keep, how long an appointment is, how soon and how far ahead one may be, and the hours you are open on each day. Two periods on a day is how a lunch break is written; a day with none is a day you are shut, and single days can be closed on top of that for a holiday. Then switch it on, and the line will offer times.

**The time zone is real, and this is the part worth reading.** Opening hours are kept in your own local time and turned into actual moments using the time-zone database, which means two things. Your nine in the morning stays nine in the morning after the clocks change, rather than drifting to ten for half the year. And the two awkward hours are handled deliberately rather than accidentally: the hour that does not exist when the clocks go forward is never offered, and the hour that happens twice when they go back is offered once, because a caller cannot tell the two apart and neither can whoever reads the diary afterwards.

**The agent never works a time out for itself.** It asks what is free, is told the practice's own local date and time along with the answer, and reads back the times it was given. Booking uses a code from that answer, and the moment is derived again from your opening hours before anything is written, so a time outside them cannot become an appointment however the conversation goes. A caller cannot talk the line into an appointment at three in the morning, because there is nowhere in the process for a time to be invented.

**Two callers cannot take one slot.** Every appointment is the length you set, so two either start at the same moment or do not overlap, and the database refuses the second one outright. The caller who loses is told the time has just gone and offered another, which is what a receptionist would say, rather than being told something went wrong.

Whoever books is given a short reference and told to quote it if they need to change anything. **Moving or cancelling by telephone needs two independent things to agree**: that reference, and either the number they are ringing from or the name it was booked under. Three attempts a call, and then no more on that call. A refusal never says which half was wrong, because "the reference is right but the name is not" tells somebody guessing that they are halfway there, and an appointment booked in person has no reference to quote, so it can only be changed by somebody in the office. It is worth being plain about the limit: a caller who has been given somebody else's reference and knows their name can act on it, exactly as they could with a human receptionist. Nothing on a telephone verifies who anybody is.

If you keep a list of names to check callers against, nothing is arranged for a caller until they have been checked and found clear, for the same reason nobody is put through until then.

The diary is your own working arrangements: the account it belongs to may read and change it, and so may an administrator of the platform. Somebody who may register telephone numbers sees whether a line takes bookings and how long an appointment is, and not who is coming in. Appointments are shown in the diary's own zone rather than the reader's, everywhere they appear, so an administrator in another country is never quietly shown a different time from the one the caller was told.

Two things this does not do yet. There is one diary per account rather than one per person or room, so a practice that needs to offer the right colleague's time will want more. And nobody is reminded: the appointment is in the diary and announced in your team chat when it is made, but nothing rings anybody the day before.
