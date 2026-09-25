# hypermesh-cli

Thin **Hypermesh** CLI (Hyperme.sh). Phase 1 is **Full Model** only: catalog id `llama-3.1-8b-q4`, Stripe test Checkout, then chat on the Fyber router.

Binary: `hypermesh`. Alias: `hm`.

This is a client over the existing REST lock. It is not a second control plane.

Design lock: [DESIGN.md](DESIGN.md). Product principles live in [FyberLabs/hypermesh-docs](https://github.com/FyberLabs/hypermesh-docs) — start with [customer-interfaces.md](https://github.com/FyberLabs/hypermesh-docs/blob/main/customer-interfaces.md), [full-model.md](https://github.com/FyberLabs/hypermesh-docs/blob/main/full-model.md), [payments.md](https://github.com/FyberLabs/hypermesh-docs/blob/main/payments.md), and [router.md](https://github.com/FyberLabs/hypermesh-docs/blob/main/router.md).

## Install

Go 1.22+ on the `PATH`.

```bash
git clone https://github.com/FyberLabs/hypermesh-cli.git
cd hypermesh-cli
make build          # bin/hypermesh and bin/hm
make test
make install        # PREFIX=/usr/local (override as needed)
```

Or:

```bash
go install github.com/FyberLabs/hypermesh-cli/cmd/hypermesh@latest
go install github.com/FyberLabs/hypermesh-cli/cmd/hm@latest
```

Private module: use a GitHub account that can read this repo.

## Auth

One org API key from api-keys (`purpose: renter`) hits both `api.test` and `chat.test`. Headers: `X-Api-Key` and `X-Tenant-ID`.

Do not use `hm_dev_`, `hm_rtr_`, or `hm_site_` keys. Those are host, router, and site credentials.

```bash
hypermesh auth login \
  --api-key "$HYPERMESH_API_KEY" \
  --tenant-id "$HYPERMESH_TENANT_ID" \
  --renter-user-id "$HYPERMESH_RENTER_USER_ID"

hypermesh auth whoami
hypermesh auth logout
```

Files:

- `~/.config/hypermesh/config.toml`
- `~/.config/hypermesh/credentials` (mode `0600`)

Defaults (override with env or `--api-base` / `--chat-base`):

| Env | Default |
|---|---|
| `HYPERMESH_API_BASE` | `https://api.test.hyperme.sh` |
| `HYPERMESH_CHAT_BASE` | `https://chat.test.hyperme.sh` |

Also: `HYPERMESH_API_KEY`, `HYPERMESH_TENANT_ID`, `HYPERMESH_RENTER_USER_ID`, `HYPERMESH_LEASE_ID`, `HYPERMESH_SUCCESS_URL`, `HYPERMESH_CANCEL_URL`, `HYPERMESH_CONFIG_DIR`.

`whoami` reads local config only. The CLI does not invent an identity endpoint.

## Phase 1 flow

1. See the public catalog and classes (no key required).
2. List renter-safe hosts and copy `device_id` (the plane UUID). `public_label` is display only.
3. Checkout a Full Model lease (`kind=p2_loaded_model`, `catalog_id=llama-3.1-8b-q4`, `purpose=renter`, `device_id=<UUID>`). The CLI does not pick a host. Missing `--device-id` fails before POST.
4. Pay Stripe **test** Checkout. The CLI prints `lease_id` and `checkout_url` immediately and opens the URL unless `--no-open`.
5. When the lease is `active`, chat on the Fyber router with the same key plus the paid lease ticket.

```bash
hypermesh catalog
hypermesh catalog show llama-3.1-8b-q4
hypermesh classes

hypermesh hosts
hypermesh hosts --catalog-id llama-3.1-8b-q4

hypermesh checkout \
  --device-id "$DEVICE_ID" \
  --renter-user-id "$HYPERMESH_RENTER_USER_ID" \
  --success-url "https://hyperme.sh/ok" \
  --cancel-url "https://hyperme.sh/cancel" \
  --no-open \
  --wait

hypermesh lease list
hypermesh lease show "$LEASE_ID"

hypermesh prompt --script --lease-id "$LEASE_ID" "hello"
hypermesh chat --script --lease-id "$LEASE_ID" --message "hello"
hypermesh completions create --script --lease-id "$LEASE_ID" --model llama-3.1-8b-q4 --message "hello"

# Bash: stdout is only the model text. Exit status is 1 on failure.
text=$(hypermesh prompt --script --lease-id "$LEASE_ID" "hello")

# PowerShell calls the same binary. It does not open its own HTTP client.
./scripts/hypermesh-prompt.ps1 -LeaseId "$LEASE_ID" "hello"

hypermesh lease complete "$LEASE_ID"
```

`--json` works on every command except together with `--script`. Failures exit `1` (not the HTTP status). Prompt bodies are not logged.

`--script` is the non-interactive mode. Stdout is only the assistant text. Errors and logs stay on stderr, so a shell capture does not mix them into the model text. An empty assistant message is a failure. `scripts/hypermesh-prompt.ps1` execs `hypermesh prompt --script`; it is not a second HTTP client.

Chat is `POST $HYPERMESH_CHAT_BASE/v1/chat/completions` with `lease_id` in the body and `X-Hypermesh-Lease-Id` / `X-Lease-Id`. The CLI never calls `POST /api/v1/hypermesh/renter/chat/completions` (always-409 stub).

Lease status: `offered` → `paid` → `starting` → `active` → `ended` | `failed` | `refunded`. `--wait` polls until `active` or `failed`.

## Out of scope (v0)

- Your Model / BYOM / box rent / load
- Host enroll, `device_secret`, WireGuard keys, host IPs
- MHS, clustering, public tok/s
- A second login (OIDC, SIWE) in this binary
- Invented catalog ids beyond `llama-3.1-8b-q4` as the Phase 1 default
- Crypto / USDC pay, fake pay, or payment bypass

## Commands

| Command | Route |
|---|---|
| `auth login\|whoami\|logout` | local config |
| `catalog` / `catalog show` | `GET /api/v1/hypermesh/catalog` |
| `classes` | `GET /api/v1/hypermesh/classes` |
| `hosts` | `GET /api/v1/hypermesh/renter/hosts` |
| `checkout` | `POST /api/v1/hypermesh/leases` |
| `lease list\|show\|complete` | `GET/POST /api/v1/hypermesh/leases[/{id}[/complete]]` |
| `chat` / `prompt` / `completions create` | `POST {chat base}/v1/chat/completions` (`--script`: stdout is model text, exit 1 on failure) |
