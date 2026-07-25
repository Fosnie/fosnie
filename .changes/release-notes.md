### Added

- Added a desktop application for Windows and macOS: pair it with your instance using a code from your profile, then work in it with reliable streaming, system notifications and signed updates you approve before they install.
- Added connected folders: connect a folder on your computer to the desktop app, and an agent in a chat can work in it, with reading allowed outright and every change, deletion or command shown for you to agree to first and reversible afterwards.
- Added automations that run against a connected folder on your own machine, with missed runs surfaced and file changes paused for your approval.
- Added a pinned plan above the stream, an end-of-turn summary of the files changed and commands run, an actions-only view, and made a waiting approval impossible to miss: a decision taken on one device settles the request on the others at once, the desktop app carries a taskbar count of approvals waiting, and a failed task raises a notification.
- Added device pairing, so a native client can be signed in to an instance with its own per-machine token, listed and revoked from your profile.
- Added support for the app to run against an instance it is not served by, signing in with a paired device token instead of a browser session.
- Added three ways to get the desktop application: download it from us, have your own instance hand it out and update it, or build it from source.

### Changed

- On computers that confine the desktop agent's commands, an everyday command that stays in the connected folder and does not need the internet now runs without asking each time, while commands that reach the network or change files elsewhere are still confirmed.
- A command run by the desktop agent that overruns its time limit is now stopped together with any programs it started, rather than leaving them running.
- The desktop client now carries the Fosnie mark everywhere Windows shows it, sends notifications under the Fosnie name and icon, draws its own title bar on Windows, and opens filling the screen the first time before remembering the size and position you leave it at.

### Fixed

- Fixed the sidebar being squeezed into a stub column with its labels cut off when the window is narrow, such as a desktop window snapped to half a screen: it now steps aside and opens over the page on request, as it already did on a phone.

Full notes: https://docs.fosnie.dev/changelog/v0.5.0
