# Observability — Metrics & Tracing

## Quick start

Set `log_level` and `log_format` in your config YAML (or override via `.env`):

```yaml
observability:
  log_format: json          # json | pretty  (default: pretty)
  log_level: "${RUST_LOG:-info}"  # any tracing-subscriber directive
```

Start the binary, then scrape `/metrics` for Prometheus text:

```
curl http://localhost:8080/metrics
```

---

## Metrics catalog

All metrics are emitted via the `metrics` facade and exported in Prometheus text format at `GET /metrics`.

### HTTP layer (end-to-end, at the edge)

These are recorded by `ObservabilityLayer` — they cover the full round-trip from when the request arrives at the proxy to when the response is returned to the caller.

| Metric | Type | Labels | Description |
|---|---|---|---|
| `http_requests_total` | counter | `route`, `method`, `status` | Total requests handled. `route` is the backend name (e.g. `"api"`), never the raw path. |
| `http_request_duration_seconds` | histogram | `route`, `method` | End-to-end latency in seconds. |
| `http_requests_in_flight` | gauge | — | Requests currently being processed. Decremented via RAII on response or client disconnect. |

**Buckets** for `http_request_duration_seconds`:
`0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0` seconds.

### Upstream layer (per backend hop)

Recorded inside `forward_once` — these measure only the network call to the upstream server, not the full proxy overhead.

| Metric | Type | Labels | Description |
|---|---|---|---|
| `upstream_requests_total` | counter | `backend`, `status` | Upstream attempts. `status` is the HTTP status code on success, or `"error"` on connect failure. |
| `upstream_request_duration_seconds` | histogram | `backend` | Upstream hop latency in seconds. |
| `upstream_errors_total` | counter | `backend`, `kind` | Connect/transport failures. `kind = "connect"` today; more kinds added in F3 (timeouts, retries). |

**Cardinality discipline:** labels are always bounded identifiers (`route`/`backend` name, HTTP method, status code). Raw path, query string, and client IP are never used as labels.

---

## Tracing spans

Two spans are opened per proxied request, nested:

```
request{method, path, request_id, client_ip, route*, status*, latency_ms*}
  └── upstream{backend, server_addr, attempt, status*, upstream_latency_ms*}
```

Fields marked `*` are declared `Empty` at span creation and filled in after the operation completes via `Span::record`.

### `request` span (INFO)

Opened by `ObservabilityLayer` at the outermost layer — before hop-by-hop stripping, before routing to the handler. Covers the full proxy handling time.

| Field | When set | Value |
|---|---|---|
| `method` | creation | HTTP method |
| `path` | creation | Request path (raw, for tracing only — not used as a metric label) |
| `request_id` | creation | Inbound `X-Request-Id` if present and non-empty, else a generated UUID v4 |
| `client_ip` | creation | Peer socket IP |
| `route` | inside `proxy_request`, after LB selection | Backend name from config |
| `status` | after response returns | HTTP status code |
| `latency_ms` | after response returns | End-to-end ms |

### `upstream` span (DEBUG)

Opened by `#[instrument]` on `forward_once`. Only visible at `debug` level or lower.

| Field | When set | Value |
|---|---|---|
| `backend` | creation | Backend name |
| `server_addr` | creation | `host:port` of the selected upstream replica |
| `attempt` | creation | `1` today; retries (F3) will increment this |
| `status` | after upstream responds | HTTP status code |
| `upstream_latency_ms` | after upstream responds | Network hop ms |

**Security:** `forward_once` uses `#[instrument(skip_all)]`. No request headers (including `Authorization` or `Cookie`) are captured as span fields. Only the explicit safe fields above are recorded.

---

## Log levels

| Level | Events |
|---|---|
| `ERROR` | Upstream connect/transport failure (the 502 cause); bind failure; startup config error |
| `WARN` | 5xx access-log line |
| `INFO` | Server lifecycle (listening/shutdown); 2xx–4xx access-log line |
| `DEBUG` | `upstream` span; LB selection |
| `TRACE` | Reserved for header-level detail — never `Authorization`/`Cookie` |

The access-log event is emitted once per request at span close, at the level determined by the response status class.

---

## Request ID

`X-Request-Id` is the join key across all log lines for a single request.

- If the inbound request carries a non-empty `X-Request-Id`, it is preserved and propagated.
- Otherwise a UUID v4 is generated.
- The value is recorded on the root `request` span and echoed back in the response `X-Request-Id` header.

---

## OTLP (future — F6)

A composition seam is left in `telemetry::init` for an OTLP layer. When F6 lands, add `tracing-opentelemetry` + `opentelemetry-otlp` and set `otlp_endpoint` in the config:

```yaml
observability:
  otlp_endpoint: "http://collector:4317"
```

Until then, the field is parsed but ignored.
