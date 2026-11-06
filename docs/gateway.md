# The HTTP/JSON gateway

The coordinator's private listener serves the operator API twice: as gRPC, and as plain HTTP with
JSON bodies, the gateway. `hostkeeper ctl --transport gateway` and the operator web app
([docs/ui-serving.md](ui-serving.md)) call the gateway. Both ways reach the same handlers with
the same checks; the gateway adds no rule of its own. Every command and request message is in
[docs/ctl.md](ctl.md).

## Route

One route per unary method of the `hostkeeper.operator.v1` services (`Hosts`, `Releases`,
`Services`, `Enrollment`, `Audit`):

```
POST /<package>.<Service>/<Method>
```

for example `POST /hostkeeper.operator.v1.Releases/Converge`. A client sends `Content-Type:
application/json` (the gateway reads the body as JSON whatever the header says); the response has
that type. Only the private listener (`--private-listen`)
serves the gateway; the heartbeat listener never does.

## Authentication

The caller sends `Authorization: Bearer <token>`, as a gRPC call sends it in its `authorization`
metadata, and the token is checked before the body is read. `Releases.SetDesired` takes CI's
OIDC token or an operator token; `Releases.Converge` takes CI's OIDC token only. Each CI token is
good once, and an operator token spends none. With CI's token `SetDesired` sets only the services
the admitting allow-list entry names. With an operator token it sets any service the coordinator
manages, and the set is one audit entry, `set_desired`, under the token's name (the service, and
as its reason the version, followed by `with package digest <digest>` when the call named one).
The JSON route `POST /v1/desired` stays CI's only. Every other method takes an operator token
only. TLS authenticates the server only; there is no client certificate and no cookie.

## Request and response

Both are the method's message in the canonical proto3 JSON mapping:

- field names are lowerCamelCase (`nodeId`, `maxUpdating`);
- 64-bit integers are strings (`"sinceSeq": "12"`);
- enums are their names (`"HEALTH_HEALTHY"`);
- bytes are base64;
- a field with its default value may be left out of a response, and a request.

A request body is one JSON document of at most 4 KiB with nothing after it. A field the message
does not have, a value of the wrong type, malformed JSON, a body that is too large and a body
that does not arrive whole are all refused as `INVALID_ARGUMENT` with the reason
`invalid_request`. A response is 200 with the response message.

## Errors

Any other answer of a gateway call is a JSON body with two strings:

```json
{"code": "FAILED_PRECONDITION", "reason": "below_min_version"}
```

`code` is the name of the gRPC status code and `reason` is the status's message: the lowercase
token of the service's refusal enum (`HostsRefusal`, `ReleasesRefusal`, and so on), the same as a
gRPC call's message. The HTTP status follows the code:

<!-- status-table -->
| Code | HTTP status |
|---|---|
| `OK` | 200 |
| `CANCELLED` | 499 |
| `UNKNOWN` | 500 |
| `INVALID_ARGUMENT` | 400 |
| `DEADLINE_EXCEEDED` | 504 |
| `NOT_FOUND` | 404 |
| `ALREADY_EXISTS` | 409 |
| `PERMISSION_DENIED` | 403 |
| `RESOURCE_EXHAUSTED` | 429 |
| `FAILED_PRECONDITION` | 400 |
| `ABORTED` | 409 |
| `OUT_OF_RANGE` | 400 |
| `UNIMPLEMENTED` | 501 |
| `INTERNAL` | 500 |
| `UNAVAILABLE` | 503 |
| `DATA_LOSS` | 500 |
| `UNAUTHENTICATED` | 401 |
<!-- /status-table -->

Only the HTTP status 200 is a success.

## What is never served

- No other route. A path that is not a method of `hostkeeper.operator.v1` is not a gateway path:
  the methods of `hostkeeper.agent.v1` and `hostkeeper.local.v1` answer 404, and so does any
  other path the private listener does not serve.
- Only `POST`. Any other method on a gateway path, `OPTIONS` and `GET` included, answers 405.
- No CORS: no answer carries an `Access-Control-*` header, so a page from another origin cannot
  call the gateway.
- No gRPC-web, no streaming and no reflection. A gRPC call is HTTP/2 with `Content-Type:
  application/grpc` and does not go through the gateway.
- A method the coordinator has no handler for yet would answer `UNIMPLEMENTED` (501) with an
  empty reason after its caller is checked. Every method of `hostkeeper.operator.v1` has one
  today.
