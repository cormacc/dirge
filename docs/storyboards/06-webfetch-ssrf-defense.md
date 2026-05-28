# Webfetch SSRF defense

The agent invokes `webfetch` with a URL. Three layers of defense plus
a redirect guard refuse private/loopback/link-local targets. Common
SSRF payloads — raw metadata IPs, alt-form IPv4 encodings, DNS
rebinding to a private address — are all blocked. The user opts out
with `DIRGE_WEBFETCH_ALLOW_PRIVATE=1` for local development.

## Flow

1. Agent calls `webfetch` with a URL list.
2. Layer 1 — literal-host check: extracts host (bracket-aware for
   IPv6), checks hostname blocklist, parses as `IpAddr`, falls back
   to `parse_alt_ipv4` for decimal/octal/hex encodings. Blocks if
   the literal resolves to private/loopback/link-local.
3. Layer 2 — pre-request DNS resolution: skips IP literals, calls
   `tokio::net::lookup_host`, blocks if any resolved address is
   private/loopback.
4. Layer 3 — custom `dns_resolver` on the `reqwest::Client`: every
   TCP connect (initial and redirects) re-resolves and re-validates.
   Catches DNS rebinding past the layer-2 cache.
5. Layer 4 — redirect policy: re-runs `validate_url_host_safety` on
   every hop; max 10 hops.
6. If allowed, the fetch proceeds; otherwise the tool returns an
   error that surfaces to the LLM as a tool result.

## Implementation

- `src/agent/tools/webfetch.rs::WebFetchTool::call` — builds the
  `reqwest::Client` with the SSRF-defending config and iterates URLs.
- `src/agent/tools/webfetch.rs::validate_url_host_safety` — layer 1.
- `src/agent/tools/webfetch.rs::parse_alt_ipv4` — decimal/octal/hex
  IPv4 fallback parser.
- `src/agent/tools/webfetch.rs::is_private_or_loopback` and
  `is_ipv4_mapped_private` — predicates used across all layers.
- `src/agent/tools/webfetch.rs::resolve_and_validate_host` — layer 2;
  honors `DIRGE_WEBFETCH_ALLOW_PRIVATE`.
- `src/agent/tools/webfetch.rs::ValidatingResolver` — layer 3; impls
  `reqwest::dns::Resolve`.
- `reqwest::redirect::Policy::custom` in `WebFetchTool::call` —
  layer 4 hop validation.

## Edge cases

- `DIRGE_WEBFETCH_ALLOW_PRIVATE=1`: opt-out honored at every layer.
- Unresolvable hostname: layer 2 returns `Ok(())` so reqwest surfaces
  the canonical network error.
- Mixed resolved addresses (one public, one private): layer 2 rejects
  on any private address — reqwest could pick either at connect time.
- Public hostname → 302 → private IP: passes layers 1-3, caught by
  layer 4.
- Cloudflare-fronted hostname: resolves to a public CF IP, connects
  normally.
