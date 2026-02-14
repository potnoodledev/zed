# Running Zed Collab Server with Docker Compose

## Overview

The Zed collab server can be run as a fully self-contained stack using Docker Compose. All services are added to the existing `compose.yml` under the `collab-server` profile, so the default local dev workflow (`docker compose up` for just minio + livekit) is unaffected.

## Quick Start

```bash
docker compose --profile collab-server up --build
```

This builds the collab server from `Dockerfile-collab` and starts all dependencies. First build takes several minutes (Rust compilation); subsequent runs use the Docker cache.

## Architecture

Six services, four of which are specific to the collab-server profile:

| Service | Image | Profile | Purpose |
|---------|-------|---------|---------|
| `blob_store` | `minio/minio` | default | S3-compatible object storage |
| `livekit_server` | `livekit/livekit-server` | default | WebRTC media server |
| `postgres` | `postgres:16-bookworm` | collab-server | Database |
| `migrate` | `postgres:16-bookworm` | collab-server | Applies schema SQL then exits |
| `createbuckets` | `minio/mc` | collab-server | Creates required MinIO buckets then exits |
| `collab` | Built from `Dockerfile-collab` | collab-server | The collab server itself |

### Startup Order

```
postgres (wait for healthy) ──> migrate ──> collab
blob_store ──> createbuckets ─────────────/
livekit_server
```

The `collab` service only starts after both `migrate` and `createbuckets` complete successfully.

## What Each Init Service Does

### migrate

Runs `psql -f /schema.sql` against the postgres database, applying the schema from `crates/collab/migrations/20251208000000_test_schema.sql`. The collab binary does not run migrations automatically -- in production this is handled externally.

### createbuckets

Uses the `minio/mc` CLI to create two buckets that collab expects:
- `the-extensions-bucket` -- for Zed extensions
- `zed-crash-reports` -- for crash report uploads

## Issues Encountered

### 1. Existing `compose.yml` conflict

The project already had a `compose.yml` with `blob_store` and `livekit_server` services. Docker Compose prefers `compose.yml` over `docker-compose.yml`, so creating a separate `docker-compose.yml` caused a warning and was ignored.

**Fix:** Added the new services directly to `compose.yml` using `profiles: [collab-server]` so they only start when explicitly requested.

### 2. Underscore in hostname (DNS resolution failure)

The existing services use `container_name: blob_store` and `container_name: livekit_server`. Underscores are invalid in DNS hostnames (RFC 952), which caused `minio/mc` to fail with "invalid hostname" when trying to connect to `http://blob_store:9000`.

**Fix:** Added `hostname: blobstore` and `hostname: livekit` to those services, and referenced the hyphen-free hostnames in the collab-server services.

### 3. Missing database schema

The collab binary's `serve` command does not run database migrations. It connects to the database and immediately queries tables like `notification_kinds`, which fails if the schema doesn't exist.

**Fix:** Added a `migrate` init service that applies the schema SQL using `psql` before collab starts.

### 4. Zed client authentication against local server

The Zed client uses `ZED_IMPERSONATE` + `ZED_ADMIN_API_TOKEN` to skip browser-based GitHub OAuth and authenticate directly. This calls `POST /internal/users/impersonate` on the server. However, this endpoint only existed in the separate Zed cloud service, not in the collab server.

Without the endpoint, two failure modes occurred:
- **Without `ZED_ADMIN_API_TOKEN`:** Zed falls through to browser OAuth, which opens `http://localhost:8080/native_app_signin` -- a page the collab server doesn't serve (404).
- **With `ZED_ADMIN_API_TOKEN`:** Zed calls `/internal/users/impersonate` which returned 404 since the route didn't exist on collab.

**Fix:** Added the `/internal/users/impersonate` endpoint to `crates/collab/src/api.rs`. The handler looks up the user by GitHub login, creates an access token, and returns `{ user_id, access_token }`. Also updated the `validate_api_token` middleware to accept both `Bearer` and `token` prefixes in the Authorization header (the client sends `Bearer`, the existing middleware only accepted `token`).

### 5. Postgres health check retries too low

On restart after unclean shutdown, Postgres needs time to recover (fsync). The original 10 retries at 2s intervals wasn't enough, causing dependent services to fail with "dependency failed to start: container is unhealthy".

**Fix:** Increased health check retries from 10 to 30.

## Connecting a Zed Client

After `docker compose --profile collab-server up --build`, run Zed with:

```bash
ZED_SERVER_URL=http://localhost:8080 \
ZED_RPC_URL=http://localhost:8080/rpc \
ZED_IMPERSONATE=your-github-username \
ZED_ADMIN_API_TOKEN=secret \
./target/debug/zed
```

The `ZED_IMPERSONATE` value must match a GitHub username in the seed data (`crates/collab/seed.default.json`). The `ZED_ADMIN_API_TOKEN` must match the `API_TOKEN` env var on the collab server (default: `secret`).

### Running multiple instances

To test collaboration, launch a second instance as a different user:

```bash
ZED_SERVER_URL=http://localhost:8080 \
ZED_RPC_URL=http://localhost:8080/rpc \
ZED_IMPERSONATE=nathansobo \
ZED_ADMIN_API_TOKEN=secret \
ZED_STATELESS=1 \
./target/debug/zed
```

`ZED_STATELESS=1` prevents the second instance from sharing window state with the first.

## Verification

```bash
# Version endpoint
curl http://localhost:8080/
# Returns: zed:all v0.44.0 ()

# Health check
curl http://localhost:8080/healthz
# Returns: ok
```

## Data Persistence

- **Postgres** data is stored in the `postgres_data` named volume
- **MinIO** data is stored in `./.blob_store/` (bind mount, same as local dev)

To reset everything:

```bash
docker compose --profile collab-server down -v
```
