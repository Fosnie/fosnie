---
kind: added
bump: minor
---

# Getting the desktop application

## changelog

Added three ways to get the desktop application: download it from us, have your own instance hand it out and update it, or build it from source.

## site

Your instance can now hand out the desktop application itself. Upload the installer once and everybody downloads it from your own server, with installed applications taking their updates from there too, so getting and keeping the desktop application never involves reaching us at all.

## detail

The desktop application used to arrive by whatever route somebody arranged. It now has three: a signed download from us, an installer your own instance serves to its own users, and a documented build from source. An installation that hosts its own copy never reaches us at all, because paired applications ask it for updates before anywhere else, so a closed network stays current entirely from inside itself and an administrator decides when the version moves rather than everybody updating whenever we publish. Every installer is published with its SHA-256 so a vetting pipeline can confirm that the file it received is the file that was published.
