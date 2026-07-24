---
kind: changed
bump: minor
---

# Fewer interruptions for commands the agent runs on your computer

## changelog

On computers that confine the desktop agent's commands, an everyday command that stays in the connected folder and does not need the internet now runs without asking each time, while commands that reach the network or change files elsewhere are still confirmed.

## site

The desktop agent now interrupts you far less. On a computer that runs its commands inside a protected boundary, ordinary commands that stay inside your connected folder and do not need the internet simply run, so you are asked only about the things that genuinely warrant a look: a command that needs network access, or anything that would change files outside the folder.

## detail

Until now the desktop agent asked you to confirm every command before it ran. That was the safe default, but it meant confirming a great many commands that could not do any harm.

On a computer where the agent's commands run inside an enforced boundary, that boundary already guarantees a command can only touch the connected folder and, unless the command asks otherwise, cannot reach the network. There is nothing left to weigh for such a command, so it now runs without interrupting you.

You are still asked about the cases that matter. A command that declares it needs the internet is shown to you with the choice to allow it with network access, and you can agree to a repeated one so the same command is not queried again. Anything that would change files outside the agreed folder, and every deletion, is still confirmed every time. The relaxation applies only where the boundary is actually enforced; elsewhere the agent asks exactly as it did before. An administrator who prefers the earlier behaviour can switch confirmation for every command back on for the whole deployment.
