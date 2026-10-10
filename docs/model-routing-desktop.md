# Desktop remote routing

The paired desktop client uses the same open-source Core composer, profiles,
policies, planner and ledger as browser chat. Enterprise uses that Core interface
and its existing RBAC/inference-policy restrictions. No separate desktop model
catalog or selection engine is introduced.

The initial supported scope is **general remote text conversations**. The
instance executes the permitted model, RAG and adopted server tools. Local
folders, desktop commands, file attachments, device automations, voice and
stateful tool combinations retain their existing routing boundaries. A model's
locality refers to its configured inference endpoint; choosing Local-only does
not install or expose a model on the paired machine.

## Discovery and selection

`GET /api/routing/browser-chat` uses the existing default-off
`routing.browser_chat_enabled` gate and now also advertises:

```json
{
  "schema_version": 1,
  "frame": "chat.routed.send",
  "desktop": true,
  "desktop_scope": "paired_remote_general_v1",
  "desktop_credential": "original_key_ticket_v1",
  "workspace": false
}
```

The desktop composer enables routing only when this exact supported schema,
frame and credential capability are present. The gate, configured profiles and
effective policy must also permit the request. Default, Manual and Auto,
objectives, Local-only and provider effort values use the browser contract.
The device token is supplied by the existing remote-instance request layer;
cookies are omitted. The WS bridge sends the explicit selection unchanged.
Device IDs, credential IDs, execution plans and budgets never come from the
client's routed request body.

New desktop conversations are recorded with `origin='desktop'`. Existing owned
Desktop/general conversations can be explicitly adopted while their provenance
is supported. Selection/options are saved before planning and reused when
omitted on subsequent routed requests. Browser and Desktop origins retain their
original surface; a conversation from another surface is readable but this
entry does not silently adopt it. Saved routing intent survives an unavailable
capability, provider or source. A legacy `chat.send` cannot downgrade an adopted
conversation.

Actual model/effort and completion/interruption metadata are persisted and shown
after reload through the shared messages endpoint. Shadow displays the actual
answering target, preserving the proposal separately. Models no longer visible
to the original owner are withheld from user-facing metadata.

## Original credential and cancellation

HTTP authentication captures the exact live device-key UUID along with its
device UUID. `POST /api/ws-ticket` carries both into server-owned Redis data.
The existing principal value remains `user|device` so older servers can still
decode it; a companion value records the key UUID with the same TTL. Ticket and
companion are issued atomically and consumed atomically, once. Resume tokens
preserve this binding instead of looking up whichever key is currently live
for the device.

The original device/key ownership, key expiry/revocation, device revocation,
active user and original role are checked on bound connection/resume and through
the routed request's existing live access guards. Project, agent, source and
inference-policy checks still apply. Revocation during classification or output
interrupts the pinned operation; a new key for the same device cannot substitute
for the original one. Interrupted attempts retain ledger reservations and are
never transparently replayed. Admission audit traces include the surface,
device/key UUIDs and turn/source IDs, without raw prompt text or token secrets.

Tickets/resumes minted before this binding existed retain their legacy transport
behavior. A routed request through an unbound desktop connection gets an
explicit error asking it to reconnect for a fresh ticket. A fresh connection
requires the currently valid device token. ClientHello's `client_kind` and
capability claims remain descriptive and cannot change authenticated provenance.

## Regeneration and compatibility

The independent `routing.regenerate_enabled` gate advertises desktop support
for `chat.routed.regenerate`. A completed supported answer creates a new Desktop
branch using that answer's saved selection/options, current policy/profiles and
the original request's device credential. The original history remains intact.
The request accepts no new prompt, target, budget or client selection. Editing,
restarting in place and branch continuation retain their existing refusals.

An older desktop client can use its legacy frame vocabulary against the new
server for unadopted conversations; the native pump forwards unknown response
fields and does not reconstruct model requests. A new client talking to a server
without routing discovery uses legacy behavior for ordinary unadopted chats and
offers no Auto. If it has loaded saved routed intent but the server lacks the
desktop credential capability, sending is blocked while the choice is retained.
Discovery failures other than an unsupported endpoint also block sending.

Offline acceptance uses production HTTP/WS routes against isolated PostgreSQL
and Redis, loopback ML mocks and the shared composer browser harness in synthetic
desktop mode. No real pairing, folder access, paid inference or release signing
is required. The desktop package's `frontend:build` builds the actual shared Core
frontend; this acceptance does not produce a signed native release or measure
live model quality.
