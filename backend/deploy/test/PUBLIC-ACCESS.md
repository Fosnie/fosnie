# Putting the demo on the internet — `https://chat.example.com`

**Short answer: it's not much work.** One extra container (a Cloudflare Tunnel),
two DNS hostnames, and a ~6-line Keycloak client tweak. No open firewall ports, no
TLS certs to manage, no public IP needed — Cloudflare terminates HTTPS and reaches
your box through an outbound-only tunnel. Realistic time if the domain is already
on Cloudflare: **30–60 min**, most of it the Keycloak redirect-URI step.

Why a tunnel rather than "open 443 + Caddy + Let's Encrypt": for a one-day hub
demo it's the least to go wrong — nothing inbound to firewall, certs are
automatic, and you can tear it down after. It also matches a Cloudflare-ZTNA
posture.

---

## One-off setup

### 1. Domain on Cloudflare
`example.com` must use Cloudflare nameservers (Cloudflare dashboard → add
site → update NS at your registrar). Free plan is fine.

### 2. Create the tunnel
Cloudflare dashboard → **Zero Trust → Networks → Tunnels → Create a tunnel**
(`cloudflared` type). Name it e.g. `pai-demo`. Copy the **tunnel token** it shows.

Add two **public hostnames** to the tunnel (same screen):

| Public hostname | Service |
|---|---|
| `chat.example.com` | `http://localhost:8080` |
| `auth.example.com` | `http://localhost:8081` |

(Cloudflare auto-creates the DNS CNAMEs for both.)

### 3. Token into `.env`
```bash
echo 'CLOUDFLARE_TUNNEL_TOKEN=eyJhIjoi…your-token…' >> .env
```

### 4. Patch the Keycloak realm for the public hostnames
The dev seed only allows `http://localhost/*`. Fastest path — the admin console
(no rebuild): bring the stack up (step below), open
`https://auth.example.com`, log in `admin` / `admin`, realm **fosnie**, then:

- **Clients → `fosnie-spa`** (the browser app):
  - *Valid redirect URIs*: add `https://chat.example.com/*`
  - *Web origins*: add `https://chat.example.com`
  - *Valid post logout redirect URIs*: add `https://chat.example.com/*`
- **Clients → `fosnie`** (the backend):
  - *Web origins*: add `https://chat.example.com`

Save. (You can keep the `http://localhost` entries — harmless for the demo.)

> Prefer it baked in? Edit `../keycloak/import/fosnie-realm.json` before first boot,
> adding those URIs to the two clients — but the console is faster for one demo and
> survives a realm re-import via the running DB.

---

## Launch

```bash
docker compose --env-file .env \
  -f docker-compose.test.yml -f docker-compose.public.yml up -d
```

The overlay (`docker-compose.public.yml`) adds `cloudflared` and flips the backend
+ Keycloak to the public hostnames (`public_url`/issuer become `https://…`, which
the backend's boot `validate()` requires off-loopback; Keycloak gets
`KC_PROXY_HEADERS=xforwarded` + `KC_HOSTNAME` so it builds correct https URLs).

Then just send people **`https://chat.example.com`**. Log in with a seed
user (`alice` / `alice`).

### Smoke
```bash
curl -I https://chat.example.com            # 200/302 from your box via CF
curl -s https://auth.example.com/realms/fosnie/.well-known/openid-configuration | head -c 200
```

---

## Read this before you point a hub at it

Two honest flags — neither blocks the demo, both are your call:

1. **The dev realm is on the internet.** Seed users (`alice`/`bob`/`carol`) and a
   public client secret are now reachable. Fine for a throwaway demo URL you take
   down after; do **not** leave it standing or load real data. For anything beyond
   a demo, harden per [`../keycloak/PRODUCTION-REALM.md`](../keycloak/PRODUCTION-REALM.md)
   (real users, brute-force protection, rotate the secret, `sslRequired`).
2. **The main model is `Huihui-Qwen3.6-…-abliterated` — an *uncensored* fine-tune.**
   Its safety filtering is deliberately removed, so in front of a live audience it
   can produce content you wouldn't want attached to your organisation's
   name. For a public hub demo a stock `Qwen3-…-Instruct`/`-AWQ` is the safer face;
   swap `LLM_MODEL` in `.env` (no other change). Your decision — just flagging it.

### Locking the demo down a little (optional, 5 min)
Cloudflare Zero Trust → **Access** → add an application policy on
`chat.example.com` (e.g. one-time-PIN to a allow-list of emails, or a shared
PIN). Gives you a login wall in front of the whole thing without touching the app.

---

## Teardown
```bash
docker compose -f docker-compose.test.yml -f docker-compose.public.yml down
# and delete the tunnel + DNS in the Cloudflare dashboard if it was throwaway.
```
