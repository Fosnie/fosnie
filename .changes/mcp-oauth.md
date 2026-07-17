---
kind: added
bump: minor
roadmap_id: mcp-oauth
---

# One-click MCP connections (OAuth 2.1)

## changelog

Remote MCP servers can now authenticate each user individually through OAuth 2.1, so an admin adds a server by URL and every user connects once under their own identity.

## site

Connect a remote MCP server by pasting its URL, nothing else. The platform discovers how it signs
users in and, where the server supports it, registers itself automatically. Each person then clicks
Connect once and that server's tools work for them under their own account and their own permissions.

## detail

Until now a remote MCP server used one shared credential: an admin pasted a single bearer token or API key and every user of that server connected as that one identity. An admin now registers a server by URL, the platform discovers how it authenticates and, where the server supports it, registers itself automatically, and each user clicks Connect once to authorise under their own account, so that server's tools run with the user's own identity and permissions at the provider and expiry is usually invisible. Discovery is an administrator-reviewed step and every discovered endpoint is validated (https only, cloud-metadata and link-local addresses refused, a cross-origin authorisation server only when the admin declares it), so secrets are posted only to endpoints an administrator approved. Tokens live only in encrypted columns, and a deployment with no encryption key configured refuses to store a token rather than fall back to plaintext.
