use super::*;

fn whats(found: &[Finding]) -> Vec<(usize, &str)> {
    found.iter().map(|f| (f.line, f.what.as_str())).collect()
}

#[test]
fn a_finding_names_its_file_and_line() {
    let found = token_keepers("app.js", "\nconsole.log(x);");
    assert_eq!(found[0].to_string(), "app.js:2: `console.`");
}

#[test]
fn token_keepers_finds_each_place_a_token_could_be_kept() {
    // `location` and `.search` are apart in the source: joined, the hygiene lint reads them as a
    // domain.
    let text = concat!(
        "localStorage.setItem(k, v);\nwindow.sessionStorage;\nindexedDB.open(x);\n",
        "document.cookie = t;\nconst f = location.hash;\nlocation",
        ".search;\nhistory.pushState(t);\nconsole.log(token);"
    );
    assert_eq!(
        whats(&token_keepers("pages/x.js", text)),
        [
            (1, "`localStorage`"),
            (2, "`sessionStorage`"),
            (3, "`indexedDB`"),
            (4, "`cookie`"),
            (5, "`location.hash`"),
            (6, concat!("`location", ".search`")),
            (7, "`history.`"),
            (8, "`console.`"),
        ]
    );
}

#[test]
fn a_word_needle_inside_a_longer_name_is_not_found() {
    assert!(token_keepers("x.js", "myconsole.log(x); const xcookie = 2;").is_empty());
    assert!(
        network_and_code(
            "x.js",
            "retrieval(x); makeFunction(y); reimport(z); $eval(w); do_eval(v); evaluate(t); \
             eval; eval /* never closed (x)"
        )
        .is_empty()
    );
}

#[test]
fn network_and_code_finds_urls_of_other_origins_and_code_from_text() {
    let lines = [
        "<script src=\"https://cdn.example.com/x.js\"></script>",
        "fetch(\"http://example.com/\");",
        "const u = \"//cdn.example.com/a.js\";",
        "@import 'a.css';",
        "background: url(a.png);",
        "eval(text);",
        "new Function(text);",
        "await import(name);",
        "el.innerHTML = s;",
        "el.outerHTML = s;",
        "el.insertAdjacentHTML(p, s);",
        "document.write(s);",
        "frame.srcdoc = s;",
        "a.href = 'javascript:x';",
        "new XMLHttpRequest();",
        "new WebSocket(u);",
        "new EventSource(u);",
        "navigator.sendBeacon(u, b);",
    ];
    let found = network_and_code("x", &lines.join("\n"));
    let mut lines_found: Vec<usize> = found.iter().map(|f| f.line).collect();
    lines_found.dedup();
    assert_eq!(lines_found, (1..=lines.len()).collect::<Vec<_>>());
    assert_eq!(
        whats(&network_and_code(
            "x",
            "x = '//a'; y = `//b`; f(//c); z=//d"
        )),
        [(1, "`'//`"), (1, "``//`"), (1, "`(//`"), (1, "`=//`")]
    );
}

#[test]
fn a_forbidden_call_is_found_with_blanks_before_its_parenthesis() {
    // Catches: `eval (text)`, `new Function /* x */ (text)` and the like slipping past a scan
    // that only knows the compact spelling.
    let text = "eval (text);\nnew Function /* x */ (text);\nawait import\n(name);\n\
                el.style.background = url // u\n(a);";
    assert_eq!(
        whats(&network_and_code("x.js", text)),
        [
            (1, "`eval(`"),
            (2, "`Function(`"),
            (3, "`import(`"),
            (5, "`url(`"),
        ]
    );
    assert_eq!(
        whats(&fetches("x.js", "fetch\n(u);\nwindow.fetch /* c */ (u);")),
        [(1, "`fetch(`"), (3, "`fetch(`")]
    );
}

#[test]
fn only_the_documentation_address_is_allowed_as_a_url() {
    // Catches: the allowance for the documentation link letting another address through, on its
    // own line or beside the allowed one, or another repository's pages through.
    let docs = format!("const D = \"{DOCS_LINK}\";");
    assert!(network_and_code("view.js", &docs).is_empty());
    let other = "const D = \"https://github.com/other/repo/blob/main/docs/operations/\";";
    assert_eq!(whats(&network_and_code("x.js", other)), [(1, "`https://`")]);
    let beside = format!("const D = \"{DOCS_LINK}\", E = \"https://example.com/\";");
    assert_eq!(
        whats(&network_and_code("x.js", &beside)),
        [(1, "`https://`"), (1, "`https://`")]
    );
}

#[test]
fn a_comment_or_a_relative_path_is_not_a_url() {
    let text = "// a comment\nimport { h } from \"./view.js\";\nimport * as fleet from \"./pages/fleet.js\";";
    assert!(network_and_code("app.js", text).is_empty());
}

#[test]
fn inline_code_in_html_finds_what_the_policy_refuses() {
    let html = "<script>run()</script>\n<SCRIPT type=\"module\" src=\"main.js\">x</SCRIPT>\n\
                <style>p{}</style>\n<p style = \"color: red\">x</p>\n<img src=x onerror=alert(1)>\n\
                <body onload =\"go()\">\n<script src=\"a.js\"";
    assert_eq!(
        whats(&inline_code("index.html", html)),
        [
            (1, "a script without `src`"),
            (2, "a script with a body"),
            (7, "a script with a body"),
            (3, "a `style` element"),
            (5, "an inline event handler"),
            (6, "an inline event handler"),
            (4, "an inline style"),
        ]
    );
}

#[test]
fn inline_code_in_html_is_found_in_any_case_and_across_white_space() {
    let html = "<BUTTON\n  ONCLICK = \"go()\">x</BUTTON>\n<p\tSTYLE\n= \"color: red\">x</p>\n<Script\n>run()</Script>";
    assert_eq!(
        whats(&inline_code("index.html", html)),
        [
            (5, "a script without `src`"),
            (2, "an inline event handler"),
            (3, "an inline style"),
        ]
    );
}

#[test]
fn clean_html_has_no_inline_code() {
    let html = "<html lang=\"en\">\n<script type=\"module\" src=\"main.js\"></script>\n\
                <p class=\"one\">Done once only; content== on = onfoo == bar; a style guide</p>\n<div id=\"app\"></div>\n\
                <script\n  type=\"module\"\n  SRC = \"a.js\"></script>";
    assert!(inline_code("index.html", html).is_empty());
}

#[test]
fn inline_code_in_scripts_finds_handler_properties_and_attributes() {
    let js = "el.onclick = go;\nel.onerror=go;\nel.setAttribute(\"onload\", s);\n\
              el.setAttribute( 'OnClick', s);\nif (x.onready == y) {}\nel.setAttribute(\"class\", c);\n\
              x.on.click(); h(\"p\", { on: { click: go } });\nel.once();";
    assert_eq!(
        whats(&inline_code("pages/x.js", js)),
        [
            (1, "an event handler property"),
            (2, "an event handler property"),
            (3, "an event handler attribute"),
            (4, "an event handler attribute"),
        ]
    );
}

#[test]
fn inline_code_reads_only_html_and_scripts() {
    assert!(inline_code("app.css", "a.onclick = x; <script>y</script>").is_empty());
}

#[test]
fn fetches_finds_each_call_of_fetch() {
    assert_eq!(
        whats(&fetches(
            "pages/x.js",
            "const r = await fetch(u);\nwindow.fetch(u);\nprefetch(u); fetch;"
        )),
        [(1, "`fetch(`"), (2, "`fetch(`")]
    );
}

#[test]
fn the_console_service_of_the_api_client_is_not_a_console_but_any_other_console_is() {
    // Catches: the `console.` needle refusing a call of the operator API's Console service, and
    // the exemption widening to a token logged through `console` (bare, on `window`, or behind a
    // name that only ends in `api`).
    assert!(
        token_keepers(
            "pages/host.js",
            "await ctx.api.console.power({ hostId });\nawait api.console.setBootDevice(b);"
        )
        .is_empty()
    );
    let text = "console.log(token);\nwindow.console.log(token);\nxapi.console.log(token);\n\
                console.log(ctx.api.console.power);";
    let found = token_keepers("pages/x.js", text);
    assert_eq!(
        whats(&found),
        [
            (1, "`console.`"),
            (2, "`console.`"),
            (3, "`console.`"),
            (4, "`console.`"),
        ]
    );
}

#[test]
fn the_console_service_is_listed_in_calls_json_only_as_a_function_name() {
    // Catches: `calls.json` refused for naming the Console service, and the exemption reaching
    // another file or another place in `calls.json`.
    let listed = "    { \"function\": \"console.power\", \"method\": \"x\" },";
    assert!(token_keepers("calls.json", listed).is_empty());
    assert_eq!(
        whats(&token_keepers("pages/x.js", listed)),
        [(1, "`console.`")]
    );
    assert_eq!(
        whats(&token_keepers(
            "calls.json",
            "{ \"method\": \"console.log\" }"
        )),
        [(1, "`console.`")]
    );
}

#[test]
fn a_socket_is_refused_in_every_file_but_the_entry_module() {
    // Catches: the allowance for the one socket opener letting a `WebSocket` into a page, the data
    // layer or any other module, letting a `ws://` or `wss://` address into any file the opener
    // included, or letting any other needle through in the entry module.
    let opens = "const s = new WebSocket(u);\nconst t = \"ws://example.com/x\";\nconst v = `wss://example.com/y`;";
    for path in [
        "pages/host_ssh.js",
        "pages/host.js",
        "api.js",
        "app.js",
        "view.js",
        "x",
    ] {
        assert_eq!(
            whats(&network_and_code(path, opens)),
            [(1, "`WebSocket`"), (2, "`ws://`"), (3, "`wss://`")],
            "{path}"
        );
    }
    assert_eq!(
        whats(&network_and_code(SOCKET_OPENER, opens)),
        [(2, "`ws://`"), (3, "`wss://`")]
    );
    assert_eq!(SOCKET_OPENER, "main.js");
    assert_eq!(
        whats(&network_and_code(
            SOCKET_OPENER,
            "new WebSocket(u); el.innerHTML = s; eval(t); fetch(\"https://example.com/\");"
        )),
        [(1, "`https://`"), (1, "`eval(`"), (1, "`innerHTML`")]
    );
}

#[test]
fn sockets_finds_each_call_of_websocket() {
    assert_eq!(
        whats(&sockets(
            "main.js",
            "const a = new WebSocket(u);\nnew WebSocket /* c */ (v);\nMyWebSocket(w); WebSocket;"
        )),
        [(1, "`WebSocket(`"), (2, "`WebSocket(`")]
    );
}

#[test]
fn the_opener_holds_one_websocket_call_to_the_address_socket_address_builds() {
    let one = "import { socketAddress } from \"./socket_address.js\";\n\
               const socket = new WebSocket(socketAddress(window.location, path), socketProtocols(path, csrf));";
    assert!(opener(SOCKET_OPENER, one).is_empty());
    let spaced = "new WebSocket /* c */ (\n  socketAddress(window.location, path), socketProtocols(path, csrf)\n);";
    assert!(opener(SOCKET_OPENER, spaced).is_empty());
    assert_eq!(
        SOCKET_ADDRESS, "socketAddress(window.location, path), socketProtocols(path, csrf)",
        "the argument main.js writes"
    );
}

#[test]
fn a_second_websocket_in_the_opener_is_refused() {
    // Catches: a second socket in the one file allowed to open one, whatever its address, and an
    // opener with no socket at all.
    let two = "new WebSocket(socketAddress(window.location, path), socketProtocols(path, csrf));\n\
               const b = new WebSocket(socketAddress(window.location, path), socketProtocols(path, csrf));";
    assert_eq!(
        whats(&opener(SOCKET_OPENER, two)),
        [(2, "a second `WebSocket(` call")]
    );
    assert_eq!(
        whats(&opener(
            SOCKET_OPENER,
            "const s = { WebSocket: 1 }; MyWebSocket(x);"
        )),
        [(1, "no `WebSocket(` call")]
    );
}

#[test]
fn a_socket_address_built_any_other_way_is_refused() {
    // Catches: the one socket opened to an address of the page's own building (another host, a
    // path no fixed shape checks, the scheme and host spliced by hand) or to an address made by
    // `socketAddress` from another location, and a call never closed.
    for argument in [
        "`${scheme}//${host}${path}`",
        "url",
        "socketAddress(otherLocation, path)",
        "socketAddress(window.location, path), protocols",
        "socketAddress(window.location, path)",
        "socketAddress(window.location, path), socketProtocols(otherPath, csrf)",
        "socketAddress(window.location, path), [\"hostkeeper.console.v1\", csrf]",
        "socketAddress(window.location, `/x/${path}`)",
        "socketAddress(window.location, (path))",
        "",
    ] {
        let text = format!("const socket = new WebSocket({argument});");
        assert_eq!(
            whats(&opener(SOCKET_OPENER, &text)),
            [(
                1,
                "a socket address not built by `socketAddress(window.location, path), socketProtocols(path, csrf)`"
            )],
            "{argument}"
        );
    }
    assert_eq!(
        whats(&opener(
            SOCKET_OPENER,
            "\nnew WebSocket(socketAddress(window.location, path), socketProtocols(path, csrf)"
        )),
        [(
            2,
            "a socket address not built by `socketAddress(window.location, path), socketProtocols(path, csrf)`"
        )]
    );
    let both = "new WebSocket(a);\nnew WebSocket(b);";
    assert_eq!(
        whats(&opener(SOCKET_OPENER, both)),
        [
            (
                1,
                "a socket address not built by `socketAddress(window.location, path), socketProtocols(path, csrf)`"
            ),
            (2, "a second `WebSocket(` call"),
            (
                2,
                "a socket address not built by `socketAddress(window.location, path), socketProtocols(path, csrf)`"
            ),
        ]
    );
}

/// A vendored library's text holding a needle of every list, and nothing a vendored file is
/// refused.
const NEEDLES_ONLY: &str = "/*! license: https://example.com/LICENSE */\n\
     console.warn(x); localStorage.x = 1; document.cookie;\n\
     el.innerHTML = s; fetch(u); eval(t); new Function(t);\n\
     const u = \"wss://example.com/\" + 'ws://example.org/';\n\
     if (s instanceof WebSocket || s.readyState === 1) {}\n\
     socket.onmessage = (e) => go(e); term.onData = f;";

#[test]
fn the_vendor_directory_is_exempt_from_the_needle_scan_only() {
    // Catches: the exemption missing (a licence URL or a `console.` in minified code failing the
    // scan), and the exemption reaching what a vendored file is still held to: an inline script
    // or handler in its HTML, a handler made from text in its script, and a `WebSocket(` call.
    assert!(scan_file("vendor/lib/lib.js", NEEDLES_ONLY).is_empty());
    assert!(vendored_code("vendor/lib/lib.css", "a { b: url(x.png) }").is_empty());
    let app = scan_file("pages/x.js", NEEDLES_ONLY);
    assert!(
        app.len() > 10,
        "the same text in the app is refused: {app:?}"
    );
    assert_eq!(
        whats(&scan_file(
            "vendor/lib/demo.html",
            "<script>go()</script>\n<p onclick=\"go()\">x</p>\n<p style=\"x\">y</p>"
        )),
        [
            (1, "a script without `src`"),
            (2, "an inline event handler"),
            (3, "an inline style"),
        ]
    );
    assert_eq!(
        whats(&scan_file(
            "vendor/lib/lib.js",
            "el.setAttribute(\"onclick\", s);\nthis.attach(new WebSocket(uri, protocols));"
        )),
        [(1, "an event handler attribute"), (2, "`WebSocket(`")]
    );
    assert_eq!(
        whats(&scan_file("vendor/lib/lib.css", "x { }\nnew WebSocket(u)")),
        [(2, "`WebSocket(`")]
    );
}

#[test]
fn scan_file_applies_every_rule_of_one_app_file() {
    // Catches: a rule of the app's own files left out of `scan_file`.
    let text = "console.log(t);\nfetch(\"https://example.com/\");\nel.onclick = go;";
    assert_eq!(
        whats(&scan_file("pages/x.js", text)),
        [
            (1, "`console.`"),
            (2, "`https://`"),
            (3, "an event handler property"),
        ]
    );
}
