# Backup & Disaster Recovery

How the PAI Platform is backed up and restored. The scripts live in
[`scripts/backup.sh`](scripts/backup.sh) and [`scripts/restore.sh`](scripts/restore.sh);
the schedule is [`systemd/pai-backup.timer`](systemd/pai-backup.timer).

## What is backed up

| Store | Contents | How | Required |
| --- | --- | --- | --- |
| **Postgres** | All app data **+ the audit hash-chain** | `pg_dump -Fc` | yes |
| **Qdrant** | RAG vectors (every collection) | snapshot API → download | if RAG used |
| **Storage** | On-disk files: `documents/ workspace/ artefacts/ exports/ branding/ skills/ prompts/` | `tar -z` | if files stored |
| Redis | sessions, WS tickets, presence — **ephemeral** | not backed up | — |

A bundle is a timestamped directory `pai-backup-<UTC>/` containing
`postgres.dump`, `qdrant/<collection>.snapshot`, `storage.tar.gz`, a
`manifest.json`, and `sha256sums` (the integrity anchor). With
`PAI_BACKUP_AGE_RECIPIENT` set the whole bundle is `age`-encrypted to a single
`.tar.age` (the key stays inside the perimeter — zero-egress).

## Configure

Create `/etc/pai/backup.env` (read by the systemd unit):

```sh
PAI_DB_URL=postgres://pai:PASS@127.0.0.1:5432/pai
PAI_QDRANT_URL=http://127.0.0.1:6333
PAI_STORAGE_DIRS="/var/lib/pai/documents /var/lib/pai/workspace /var/lib/pai/artefacts /var/lib/pai/exports /var/lib/pai/branding /var/lib/pai/skills /var/lib/pai/prompts"
PAI_BACKUP_DIR=/var/backups/pai
PAI_BACKUP_RETAIN=14
# PAI_BACKUP_AGE_RECIPIENT=age1...      # optional encryption
```

Enable the schedule:

```sh
install -m 0755 deploy/scripts/backup.sh deploy/scripts/restore.sh /opt/pai/deploy/scripts/
cp deploy/systemd/pai-backup.{service,timer} /etc/systemd/system/
systemctl enable --now pai-backup.timer
systemctl start pai-backup.service        # take one now; check `journalctl -u pai-backup`
```

## Targets

- **RPO** (max data loss): the backup interval — daily by default (`OnCalendar`
  in the timer). Tighten to hourly for a smaller window. Sub-hour / zero-loss
  needs Postgres PITR (WAL archiving) — see *Next tier*.
- **RTO** (time to restore): dominated by `pg_restore` + Qdrant recover + un-tar;
  minutes for small/medium datasets.

## Restore (DR)

Restore order is **Postgres → Qdrant → storage** — the script enforces it.

```sh
# (decrypt first if encrypted:  age -d -i key.txt bundle.tar.age | tar -x)
PAI_DB_URL=postgres://pai:PASS@HOST:5432/pai \
PAI_QDRANT_URL=http://HOST:6333 \
  deploy/scripts/restore.sh /var/backups/pai/pai-backup-<UTC>   # add --force to overwrite a populated DB
```

The script **verifies `sha256sums` first** and refuses a populated database
without `--force`.

### Mandatory post-restore gate — verify the audit chain

The audit log is an append-only SHA-256 hash-chain (regulated requirement). After
restore, **before** putting the platform back in service:

1. Start the backend against the restored database.
2. Hit the admin audit verifier — `GET /api/admin/audit/export` returns the chain
   status (backed by [`audit::verify::verify_chain`](../src/audit/verify.rs)).
3. **A `bad` status aborts cutover** — it means the dump was tampered with or
   corrupted in transit. Restore from an earlier known-good bundle instead.

Then smoke-test: a known chat opens, a known document renders, RAG retrieval
returns results (Qdrant restored).

## Test the restore regularly

A backup you have never restored is not a backup. Quarterly, restore the latest
bundle into a scratch DB + Qdrant, run the audit-chain verify, and confirm a
sample chat/document survives. Record the run.

## Next tier (out of scope here, tracked)

- **Postgres PITR**: WAL archiving (e.g. `pgBackRest`/`wal-g`) for near-zero RPO.
- **Offsite/immutable copies**: replicate encrypted bundles to a second location
  *inside the client perimeter* (zero-egress forbids public cloud unless the
  client explicitly allows it).
