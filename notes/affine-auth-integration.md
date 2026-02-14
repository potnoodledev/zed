# AFFiNE Auth Integration - Issues & Fixes

## Overview

Added AFFiNE as an alternate authentication provider to Zed's collab server alongside GitHub. Users can sign in with either provider through a browser-based flow served by the collab server itself.

## Issues Encountered

### 1. Database: `ON CONFLICT` with partial unique index

**Problem:** The `create_user` function uses `ON CONFLICT (github_user_id)` for upserts. After making `github_user_id` nullable, we created a partial unique index (`WHERE github_user_id IS NOT NULL`). PostgreSQL does not support `ON CONFLICT` with partial unique indexes.

**Error:** `there is no unique or exclusion constraint matching the ON CONFLICT specification`

**Fix:** Reverted to a regular (non-partial) unique index. PostgreSQL already treats NULLs as distinct in unique indexes, so multiple rows with `NULL` github_user_id are allowed without a partial index. Also updated `create_user` to only add the `ON CONFLICT` clause when `github_user_id.is_some()`.

### 2. Empty env vars treated as `Some("")` by envy

**Problem:** Docker Compose sets `GITHUB_CLIENT_ID: ${GITHUB_CLIENT_ID:-}` which resolves to an empty string when unset. The `envy` crate deserializes this as `Some("")`, causing `is_some()` to return `true` and showing auth buttons for unconfigured providers.

**Fix:** Changed checks from `.is_some()` to `.is_some_and(|s| !s.is_empty())`.

### 3. Client used wrong URL for callback

**Problem:** Initially used `zed_dot_dev_url()` for constructing OAuth callback URLs, which points to `http://localhost:3000` (the separate Zed cloud service), not the collab server itself.

**Fix:** Added a `self_url` config field (defaults to `http://localhost:{http_port}`) and used it for all callback URL construction.

### 4. Client couldn't discover RPC WebSocket endpoint

**Problem:** The Zed client discovers the collab WebSocket URL by doing `GET /rpc` and expecting a 3xx redirect (production uses a proxy at `zed.dev/rpc` that redirects to `collab.zed.dev/rpc`). In the self-hosted setup, the collab server IS the RPC server, so this GET hits the auth middleware and returns 401.

**Error:** `unexpected /rpc response status 401 Unauthorized`

**Fix:** Modified `rpc_url()` in `crates/client/src/client.rs` to handle the 401 case: when the server responds with 401, use the URL directly (the server itself is the collab endpoint) instead of expecting a redirect.

### 5. Collab WebSocket connection gated on `is_staff` flag

**Problem:** `sign_in_with_optional_connect` waits for feature flags from the cloud service to determine if the user is staff before connecting to the collab WebSocket. In self-hosted setups, the cloud service is unavailable, so `FeatureFlags` is never set, `on_flags_ready` never fires, and the collab connection is never established.

**Error:** Client shows `Connected` status but `channel_store` errors with `not connected` — the WebSocket was never actually opened.

**Fix:** Changed the logic to check if `connect_to_cloud` succeeded. When the cloud service is unavailable, connect to collab directly without waiting for the `is_staff` check.

### 6. Impersonation flow requires stdout to be a PTY

**Problem:** The `authenticate()` function in `main.rs` only triggers the impersonation flow when `stdout_is_a_pty()` returns true. Launching Zed with backgrounded/redirected output skips impersonation entirely.

**Workaround:** Launch Zed from a terminal directly, or use `script -qc '...' /dev/null` to wrap the command in a PTY.

### 7. Stored credentials fail validation on restart

**Problem:** On restart, the client reads stored credentials from the keyring and validates them via `cloud_client.validate_credentials()`. In self-hosted setups, this cloud call fails with 404, causing `AuthenticationError` and forcing re-authentication through the browser on every launch.

**Fix:** When `ZED_SERVER_URL` is set (self-hosted mode) and cloud validation fails, accept stored credentials as potentially valid. They will be verified when actually connecting to the collab WebSocket.

### 8. "Sign In" button shown after authentication

**Problem:** After signing in via AFFiNE (or impersonation), the title bar still showed "Sign In" instead of the user's avatar/name. The user menu with "Sign Out" was also hidden because `current_user` was `None`.

**Root cause:** `current_user` is populated by `_maintain_current_user` in `UserStore`, which fetches user info from the cloud service (`get_authenticated_user()`). In self-hosted setups, this call returns 404, so `current_user` is never set.

**Fix:** Added a fallback in `crates/client/src/user.rs`: when the cloud service fails and the client is in `Connected` status (collab WebSocket established), fetch the user info from the collab server via the `GetUsers` RPC call instead. This populates `current_user`, which updates the title bar to show the user avatar/menu and enables the "Sign Out" option.

### 9. Browser vs server-to-server URL for AFFiNE

**Problem:** The AFFiNE sign-in URL in the browser redirect needs to be accessible from the user's browser (e.g., `http://localhost:3010`), but the server-to-server exchange call needs to use the Docker network hostname (`http://mock-affine:3010`).

**Fix:** Added `affine_public_url` config field for the browser-facing URL, separate from `affine_url` used for server-to-server calls. The sign-in page uses `affine_public_url` (falling back to `affine_url`).

## Files Modified

| File | Changes |
|------|---------|
| `crates/client/src/client.rs` | RPC URL fallback for 401, cloud validation bypass for self-hosted, collab connection without `is_staff` gate |
| `crates/collab/src/api.rs` | Sign-in page, GitHub/AFFiNE callbacks, updated impersonation |
| `crates/collab/src/lib.rs` | Config: `github_client_id/secret`, `affine_url`, `affine_public_url`, `self_url` |
| `crates/collab/src/db/tables/user.rs` | Added `affine_user_id`, `avatar_url`; `github_user_id` -> `Option` |
| `crates/collab/src/db/queries/users.rs` | AFFiNE user lookups, updated search, `create_user` ON CONFLICT fix |
| `crates/collab/src/db.rs` | `NewUserParams`: `github_user_id` -> `Option`, added `affine_user_id` |
| `crates/collab/src/rpc.rs` | `user_avatar_url` helper with AFFiNE fallback |
| `crates/collab/src/db/queries/channels.rs` | Avatar URL with AFFiNE fallback |
| `crates/collab/src/seed.rs` | Updated for new `NewUserParams` shape |
| `crates/collab/migrations/20260215000000_add_affine_auth.sql` | Migration for nullable `github_user_id`, new columns |
| `crates/collab/migrations/20251208000000_test_schema.sql` | Updated test schema |
| `compose.yml` | New env vars, mock AFFiNE service |
| `mock_affine_server.py` | Mock AFFiNE server for testing |

## Testing

```bash
# Start the stack
docker compose --profile collab-server up --build -d

# Test with browser sign-in (AFFiNE flow)
ZED_SERVER_URL=http://localhost:8080 cargo run -p zed

# Test with dev impersonation
ZED_SERVER_URL=http://localhost:8080 ZED_IMPERSONATE=nathansobo ZED_ADMIN_API_TOKEN=secret cargo run -p zed
```
