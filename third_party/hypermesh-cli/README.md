# hypermesh-cli

Thin **Hypermesh** CLI for [Hyperme.sh](https://hyperme.sh): browse the catalog, check out a lease, chat on the router, and manage local MCP config for the [desktop visor](https://github.com/FyberLabs/hypermesh-visor).

Binaries: `hypermesh` and `hm`.

```bash
git clone https://github.com/FyberLabs/hypermesh-cli.git
cd hypermesh-cli
make build    # bin/hypermesh and bin/hm
make test
make install  # PREFIX=/usr/local
```

Or: `go install github.com/FyberLabs/hypermesh-cli/cmd/hypermesh@latest` (and the same for `cmd/hm`). Needs Go 1.22+.

## Auth

Use a renter org API key and tenant id (same key for API and chat).

```bash
hypermesh auth login \
  --api-key "$HYPERMESH_API_KEY" \
  --tenant-id "$HYPERMESH_TENANT_ID" \
  --renter-user-id "$HYPERMESH_RENTER_USER_ID"

hypermesh auth whoami
hypermesh auth logout
```

Authenticated requests send:

| Header | Value |
|---|---|
| `X-Api-Key` | renter org key |
| `X-Tenant-ID` | tenant id |

Config directory: `~/.config/hypermesh` (or `HYPERMESH_CONFIG_DIR`).

| File | Purpose |
|---|---|
| `config.toml` | API / chat bases and preferences |
| `credentials` | API key (mode `0600`) |
| `mcp.json` | MCP servers |
| `mcp-profiles.json` | named MCP profiles |
| `mcp-bindings.json` | focused-app → server bindings |

## Usage

```bash
hypermesh catalog
hypermesh catalog show "$CATALOG_ID"
hypermesh classes
hypermesh hosts
hypermesh hosts --catalog-id "$CATALOG_ID"

hypermesh checkout \
  --device-id "$DEVICE_ID" \
  --catalog-id "$CATALOG_ID" \
  --renter-user-id "$HYPERMESH_RENTER_USER_ID" \
  --success-url "https://hyperme.sh/ok" \
  --cancel-url "https://hyperme.sh/cancel" \
  --no-open \
  --wait

hypermesh lease list
hypermesh lease show "$LEASE_ID"

hypermesh prompt --script --lease-id "$LEASE_ID" "hello"
hypermesh chat --script --lease-id "$LEASE_ID" --message "hello"

text=$(hypermesh prompt --script --lease-id "$LEASE_ID" "hello")
./scripts/hypermesh-prompt.ps1 -LeaseId "$LEASE_ID" "hello"

hypermesh lease complete "$LEASE_ID"
```

Pick `CATALOG_ID` from `hypermesh catalog` and `DEVICE_ID` from `hypermesh hosts`. Checkout opens Stripe Checkout unless you pass `--no-open`; `--wait` polls until the lease is ready.

With `--script`, stdout is the assistant text and failures exit `1`. Use `--json` when you want the raw response (not with `--script`).

## API map

Defaults: `HYPERMESH_API_BASE=https://api.test.hyperme.sh`, `HYPERMESH_CHAT_BASE=https://chat.test.hyperme.sh`.

| Command | Method | Path |
|---|---|---|
| `catalog` / `catalog show` | `GET` | `/api/v1/hypermesh/catalog` |
| `classes` | `GET` | `/api/v1/hypermesh/classes` |
| `hosts` | `GET` | `/api/v1/hypermesh/renter/hosts` |
| `checkout` | `POST` | `/api/v1/hypermesh/leases` |
| `lease list` | `GET` | `/api/v1/hypermesh/leases` |
| `lease show` | `GET` | `/api/v1/hypermesh/leases/{id}` |
| `lease complete` | `POST` | `/api/v1/hypermesh/leases/{id}/complete` |
| `chat` / `prompt` / `completions create` | `POST` | `{chat base}/v1/chat/completions` |

`catalog` and `classes` are public. The rest need the renter key.

### Checkout body

```json
{
  "kind": "p2_loaded_model",
  "renter_user_id": "<uuid>",
  "catalog_id": "<from catalog>",
  "success_url": "…",
  "cancel_url": "…",
  "reserved_hours": 1,
  "purpose": "renter",
  "device_id": "<from hosts>"
}
```

`device_id` is the UUID plane id from `hosts` (not `public_label`). Lease status moves `offered` → `paid` → `starting` → `active`, then `ended` / `failed` / `refunded`.

### Chat

`POST {HYPERMESH_CHAT_BASE}/v1/chat/completions` with OpenAI-shaped `model` + `messages`, plus:

- `lease_id` in the JSON body
- `X-Hypermesh-Lease-Id` and `X-Lease-Id` headers

Prompt bodies are not logged.

## Local MCP

Config for the [hypermesh-visor](https://github.com/FyberLabs/hypermesh-visor) companion (visor attaches the active profile on session open):

```bash
hypermesh mcp catalog ls
hypermesh mcp import cursor
hypermesh mcp import project
hypermesh mcp import docker
hypermesh mcp profile ls
hypermesh mcp profile create frontend
hypermesh mcp profile use frontend
hypermesh mcp profile add filesystem
hypermesh mcp profile add chrome          # also writes catalog matchers into bindings
hypermesh mcp profile gateway on          # attach only Docker MCP gateway for this profile
hypermesh mcp bindings add filesystem --wm-class Code
hypermesh mcp bindings ls
hypermesh mcp doctor
hypermesh mcp list
```

Project import merges `.hypermesh/mcp.json` or `.cursor/mcp.json` from a path (or cwd parents). Catalog entries such as `chrome` and `code` carry desktop matchers that become `mcp-bindings.json` rows when you `profile add` them. `profile gateway on` asks the visor to attach only `docker-gateway`.

| File | Shape |
|---|---|
| `mcp.json` | `{ "mcpServers": { … } }` |
| `mcp-profiles.json` | `{ "active", "profiles": { …, "gateway"?: bool } }` |
| `mcp-bindings.json` | `{ "bindings": [ { "server", "wm_class"?, … } ] }` |
| project `.hypermesh/mcp.json` | same servers shape; merged at attach time / import |

## Environment

| Env | Default |
|---|---|
| `HYPERMESH_API_BASE` | `https://api.test.hyperme.sh` |
| `HYPERMESH_CHAT_BASE` | `https://chat.test.hyperme.sh` |

Also: `HYPERMESH_API_KEY`, `HYPERMESH_TENANT_ID`, `HYPERMESH_RENTER_USER_ID`, `HYPERMESH_LEASE_ID`, `HYPERMESH_SUCCESS_URL`, `HYPERMESH_CANCEL_URL`, `HYPERMESH_CONFIG_DIR`, `HYPERMESH_VISOR_URL`.

## Commands

| Command | Role |
|---|---|
| `auth …` | local config |
| `catalog` / `classes` | public catalog |
| `hosts` | renter hosts |
| `checkout` / `lease …` | leases + Stripe Checkout |
| `chat` / `prompt` / `completions create` | router chat |
| `mcp …` | local MCP config |

Contributor notes (same repo): [DESIGN.md](DESIGN.md).
