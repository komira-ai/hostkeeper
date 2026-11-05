# Serving the operator web app

The operator web app in [`ui/`](../ui) is static files. The coordinator serves them from its
private listener, the same listener as the operator API, so the app's calls are same-origin:
there is no cross-origin access, no cookie and no session.

## Start the coordinator with `--ui-dir`

Give `hostkeeper-coordinator serve` the directory that holds the app's files, beside its other
flags:

```sh
hostkeeper-coordinator serve --private-listen <private-address> --ui-dir <path-to-ui> ...
```

The directory must exist and be a directory when the coordinator starts; otherwise the start is
refused with a message naming `--ui-dir`. Without the flag no file is served. The flag is the only
way to set it: no environment variable is read.

## Open the app

Browse to `https://<private-address>/ui/` and paste an operator token when the page asks for one.
The page keeps the token in memory only and sends it on each API call; reloading the page asks for
it again.

## What is served

- `GET` and `HEAD` of `/ui/` and of every path below it, on the private listener only. The
  heartbeat listener never serves these files.
- `/ui/` is the directory's `index.html`, and a path ending in `/` is the `index.html` of that
  directory.
- Only files ending in `.html`, `.css`, `.js`, `.json`, `.svg`, `.png` or `.ico`, of at most
  4 MiB, are served; anything else is `404`. A path that leaves the directory (`..`, an absolute
  path, their percent-encoded or backslash forms, or a symbolic link to a file outside) is `404`,
  and the file outside is never opened. Hidden files (a name starting with `.`) are `404`.
- Each file is read from disk on each request; nothing is cached in memory.
- Every answer carries `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`,
  `Referrer-Policy: no-referrer` and
  `Content-Security-Policy: default-src 'self'; frame-ancestors 'none'`. No answer of the
  coordinator carries an `Access-Control-*` header.
- The files need no token. The operator API the app calls still needs the operator token on every
  call.

## Not done here

Packaging the `ui/` directory with the coordinator is the concern of the Linux packaging, and is
not done by the coordinator or by this page.
