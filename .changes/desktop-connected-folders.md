---
kind: added
bump: minor
---

# Connected folders

## changelog

Added connected folders: connect a folder on your computer to the desktop app, and an agent in a chat can work in it, with reading allowed outright and every change, deletion or command shown for you to agree to first and reversible afterwards.

## site

The desktop app can now work in a folder on your own computer. Connect one, choose whether an agent may only read it or also change it, and it can list, read, write and run commands there. Every write, deletion and command is shown to you before it happens, nothing outside the folder can be touched, and any change can be undone.

## detail

Connect a folder to the desktop app and a chat can work in it: the agent lists what is there, reads files, and, where you allow it, writes them, deletes them, and runs commands in it. The folder is the boundary. You choose it through your system's own folder picker and set how much an agent may do: read only, read and write, or read and write without deleting. Nothing in the folder is read until you have agreed to connect it.

Reading runs without interruption. Everything that changes a file does not: a write is shown to you as the exact difference it would make, a command as the command and the folder it would run in, a deletion as the file that would go, and each waits for you to agree. You can agree to a command once, or tell it not to ask again for commands that start the same way in that folder. A command's output appears as it runs and can be stopped part way. Every write and deletion is copied aside first, so you can put a single file back or undo everything a turn did. The one thing undo does not cover, and it says so the first time you use it, is a change made by a command the agent ran.

The safeguards are not conveniences that can be turned off. A path is checked against your real filesystem and refused if it would leave the folder once links are followed, so a shortcut pointing elsewhere cannot be used to reach outside. A command inherits none of the app's own credentials. And approving still works from anywhere you are signed in: a change proposed on your desktop can be agreed to from a browser on your phone, though putting files back and stopping a command are done at the desktop, where the folder is.
