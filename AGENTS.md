# AGENTS.md — bliss

Instructions for coding agents working in this repository. Everything here was
learned the expensive way; most entries name the bug that taught it. Every
claim was fact-checked against the code, git and this machine when the file was
written — if you find one that has gone stale, fix the file in the same commit
as the code. Read the whole file before the first edit.

bliss is a VLESS + REALITY proxy client for Windows, written in Rust. Stage 1
(current) drives an external `xray-core` process and owns everything around it:
its own SOCKS5/HTTP inbound, a relay with per-connection accounting, routing,
the machine's proxy settings, and a Dioxus desktop window. Stage 3 replaces xray
with a native TLS 1.3 + REALITY + VLESS stack behind the same `Outbound` trait.
The long-form plan, in Russian, lives outside the repo at
`%USERPROFILE%\.claude\plans\cryptic-conjuring-stroustrup.md`.

---

## 0. Before anything else

- **Never read, print, paste or commit live credentials.** These hold a working
  UUID and REALITY keys in plain text:
  - `bliss.local.toml` in the repo root (gitignored by `bliss.local.*` and
    `*.local.toml` — keep it that way);
  - the real profile, `%APPDATA%\bliss\bliss.toml`;
  - the generated engine config, `%LOCALAPPDATA%\bliss\data\xray.json`
    (written from `crates/xray/src/config.rs`);
  - v2rayN's generated `binConfigs/config.json`;
  - any `vless://` link.

  The last four live outside the repository; never copy them in. To learn the
  xray config schema, read `config.rs`, not a generated file.
- **Test data is fake by construction**: reserved names (`example.net`,
  `a.example`), TEST-NET-3 addresses (`203.0.113.0/24`), private ranges, literals
  such as `1.2.3.4`, the UUID `11111111-2222-…`, and repeated-letter keys
  (`AAAA…`). Keep new fixtures in that style so nobody has to wonder whether a
  random-looking key is real.
- **The user usually has another proxy running and may be working.** v2rayN's
  `xray` typically listens on 10808 and owns the Windows system proxy. Do not
  start the GUI or `bliss run` without a reason, never change the system proxy,
  and close any window you open.
- **No bliss command is entirely side-effect free.** `bliss --help`, `check` and
  `route` make no network connections, but every invocation first runs
  `sysproxy::recover` (which rewrites the system proxy if a dead run left
  `system-proxy-backup.txt` behind), creates the work dir, and appends to
  `bliss.log`. None of them creates the profile: only the window writes it,
  when a server or the starting rules are first added.
- **Talk to the user in Russian.** Code, comments, docs and commit messages are
  in English.

## 1. Layout

| crate | package | what lives there |
|---|---|---|
| `crates/proto` | `bliss-proto` | pure data: `vless://` parsing (`server.rs`), TOML profile (`profile.rs`), routing rules as written (`routing.rs`), VLESS Encryption parameter (`encryption.rs`), credential redaction (`redact.rs`) |
| `crates/core` | `bliss-core` | engine-agnostic: inbound — SOCKS5 and HTTP on one port (`inbound.rs`, `socks5.rs`, `http.rs`), `Outbound` trait, relay, connection tracker, routing matcher (`routing.rs`), geo `.dat` decoder (`geo.rs`), system proxy (`sysproxy.rs`), port→process lookup (`process.rs`) |
| `crates/xray` | `bliss-xray` | xray config generation, process supervision in a Job Object, GitHub downloader |
| `crates/app` | `bliss` (binary) | CLI (`main.rs`), engine thread (`engine.rs`), Dioxus window (`ui.rs`), log ring buffer (`logs.rs`), clipboard |
| `crates/spike` | `bliss-spike` | renderer measurement (WebView2 vs Blitz), not in `default-members`; build with `cargo build -p bliss-spike --profile spike`; results in `docs/spike-1.0-renderer.md` |

**The seam:** `bliss-core` and `bliss-proto` never depend on `bliss-xray`; only
the `bliss` binary (`main.rs`, `engine.rs`) calls into it. Existing xray
knowledge outside `bliss-xray`, not to be extended: the `[engine] xray` path in
`profile.rs`, Xray's `tcp`/`raw` alias in `server.rs`, and the decoder for Xray's
`.dat` files in `geo.rs`.

Runtime paths on Windows:

- profile: `%APPDATA%\bliss\bliss.toml`. It does not exist until the window
  writes it, from `bliss.example.toml` (compiled into the binary), when the
  first server or the starting rules are added; bliss runs without it
- work dir: `%LOCALAPPDATA%\bliss\data` — `xray.exe`, `geoip.dat`,
  `geosite.dat`, `xray.json`, `bliss.log`, `system-proxy-backup.txt`
- default ports: inbound SOCKS 10808, HTTP 10809; xray bridge 11080
  (`crates/proto/src/profile.rs`)

## 2. Commands

```bash
cargo fmt --all
cargo clippy --all-targets   # no warnings (default members; --workspace adds the spike)
cargo test                   # default members, ~70 tests
cargo build -p bliss         # target/debug/bliss.exe
```

- One test round-trips text through the **real Windows clipboard**; the xray
  readiness tests bind loopback ports.
- `crates/proto/tests/links.rs` snapshots the whole `Debug` output of a parsed
  `ServerConfig`. Changing its fields or the redacting `Debug` impls fails that
  test. `cargo-insta` is **not installed**: insta writes a `.snap.new` next to the
  snapshot — read the diff, confirm nothing credential-shaped became readable,
  then replace the `.snap` (or rerun that one test with `INSTA_UPDATE=always`).
  Never accept a snapshot you have not read.
- A running `bliss.exe` locks the binary: close it before `cargo build`.

CLI (`bliss --help`, and `--help` on each subcommand):

```
bliss                                         open the window; the tunnel starts with it
bliss check [PATH] | check --link             validate a profile, or one link from stdin
bliss engine list [--limit N]
bliss engine install [--version VERSION] [--newest] [--via ADDR]
bliss route <TARGET> [PATH] [--process NAME]  which rule decides; no network
bliss run [PROFILE] [--server TAG] [--engine-log ENGINE_LOG]
```

## 3. Environment traps (Windows + Git Bash)

These cost the most time on this project. None of them announce themselves.

1. **Backslashes in scripted edits get interpreted, not copied.** Inside a
   non-raw Python string literal — the replacement text in a `python -c` or
   heredoc script — `\b`, `\n` and `\r` become control bytes, and the shell layer
   can collapse `\\` to `\`. Casualties: the `\b` in `%LOCALAPPDATA%\bliss` and
   `%APPDATA%\bliss\bliss.toml` became **literal backspace bytes (0x08)** — one
   in the `engine install` help text, two in a comment in `main.rs`, present in
   every commit until they were found. A text search for `%LOCALAPPDATA%liss`
   does not match across an invisible byte. Test fixtures meant to contain `\r\n`
   once got real newlines and **still passed**, because they were then testing a
   different protocol (caught before commit).
   - Use the Edit/Write tools for any text containing a backslash. If you must
     script it, build escapes with `chr(92)`.
   - Find damage by scanning **bytes**, not text: in any `.rs`, `.toml`, `.md`
     or `.css` file, a byte below 0x20 other than tab or newline is a casualty.
   - Python 3.11 rejects a backslash inside an f-string expression
     (`f"{b.count(b'\r')}"` is a `SyntaxError`); compute the value first.
2. **A multi-replacement script that fails midway loses the replacements it
   already made**, if it writes at the end. One such abort silently dropped a UI
   fix that a commit message then called done. After any scripted edit, `grep`
   for every intended change.
3. **Line endings.** `.gitattributes` has `* text=auto eol=lf`, so commits are LF
   whatever the working tree holds. The risk is exact-match edits: some tools
   write CRLF, and an edit that matched before stops matching after. To normalise
   the working tree, write this with the Write tool and run it — it reads each
   file **before** opening it for writing (opening with `'wb'` truncates first):

   ```python
   import subprocess
   out = subprocess.run(["git", "ls-files", "-z", "-m", "-o", "--exclude-standard"],
                        capture_output=True).stdout
   for name in out.split(b"\0"):
       if name.endswith((b".rs", b".toml", b".md", b".css")):
           data = open(name, "rb").read()
           if b"\r\n" in data:
               open(name, "wb").write(data.replace(b"\r\n", b"\n"))
   ```
   After a rewrite, `git status` may still list files whose `git diff` is empty:
   stale stat information, gone after `git add`.
4. **Python here prints in cp1251.** Set `PYTHONIOENCODING=utf-8` and decode
   `subprocess` output as UTF-8 explicitly.
5. **`HTTP_PROXY` and `HTTPS_PROXY` are `http://127.0.0.1:10808`.** `NO_PROXY`
   covers loopback, but not every client honours it. Scripts that talk to
   loopback — DevTools, a local test server — should bypass the proxy explicitly
   or they may hang.
6. **Git Bash rewrites arguments that look like paths** before a native Windows
   program sees them: `/bye` arrives as `C:/Program Files/Git/bye`. Double the
   slash (`//bye`, `taskkill //F //IM python.exe`) or set `MSYS_NO_PATHCONV=1`.
7. **Background processes you start keep files locked** — a local HTTP server, a
   probe script, an open bliss window. Kill them before cleaning up or building.

## 4. Commits

- Signing is on (`gpg.program = C:\Program Files (x86)\GnuPG\bin\gpg.exe`).
  **Never bypass it.** When signing fails because no agent is reachable, starting
  the Gpg4win agent has fixed it:
  `"/c/Program Files (x86)/GnuPG/bin/gpg-connect-agent.exe" //bye`.
  Git Bash also ships `/usr/bin/gpg-agent`, with a different home directory and
  socket — make sure you are not debugging that one. Confirm with
  `git log --format='%h %G? %s' -1`: it must show `G`.
- One topic per commit. When several topics are interleaved in the same files,
  snapshot the final tree, rebuild each intermediate state, build and test every
  one, and prove the last commit byte-identical to the snapshot.
- Messages explain *why*: the bug, how it was found, what the fix costs. End with
  the `Co-Authored-By` trailer the harness gives you.
- Design sources (`*.zip`) stay out of git.

## 5. House rules for code

**Resources and correctness**

- **Anything that grows over time has a hard cap and a test that fails if the
  cap is removed**: the log buffer (400 lines, `logs.rs`), recently closed
  connections (512, `tracker.rs`), HTTP request heads (16 KB, `http.rs`). The
  project exists because another client's GUI reached 7.8 GB this way.
- **A cap checked after the allocation is not a cap.** The HTTP head budget goes
  into `take` before reading, because `read_line` allocates the whole line first.
- **Declared is not implemented.** An error variant with no constructor, a
  supervision method nobody calls, a guard that cannot fail, a dependency with a
  purpose in a comment and no user — each has shipped here looking like
  protection and providing none. Current example: the workspace `Cargo.toml`
  lists `arc-swap` "to swap the routing table under live traffic", but no crate
  uses it and live swapping does not exist.
- **A comment that contradicts the code beside it is a bug.** `pump` half-closed
  because "the peer may still have something to send", while the `select!` in
  `relay` returned as soon as either direction ended and dropped both sockets; a
  client that half-closed after its request never got the response.
- **A measurement must match its label and divide by real elapsed time.** Three
  numbers on screen were wrong for exactly these reasons.
- **Two reads under two locks are two moments.** The tracker hands out one
  `table()` taken under both locks, in the order `close` takes them.
- **`tokio::time::interval` defaults to `MissedTickBehavior::Burst`**: after the
  loop blocks (an engine start takes seconds) it fires a run of catch-up ticks,
  which once flushed the traffic chart with zeros. Every interval in the engine
  loop sets `Delay`; do the same.
- **A `Semaphore` created inside a function limits one call, not the program.**
  The ping sweep's permits live in a long-lived struct.

**Security and privacy**

- **Credentials are redacted at the type**: custom `Debug` for `UserId`,
  `PublicKey`, `VlessEncryption` and `EncryptionKey`. The UUID and public key are
  guarded by `debug_does_not_leak_credentials`; the encryption key only by the
  `parsed_reality_link_matches_snapshot` snapshot.
- **Names are never resolved locally for proxied traffic** — `Target` keeps
  domains as names so DNS does not leak. Direct routes resolve locally, on
  purpose. Consequence: `geoip` rules only match connections that name an
  address; sites by country need `geosite`.

**Inbound invariants — keep them in every new ingress (UDP ASSOCIATE, TUN)**

- **An address spelled as a name is an address.** Programs send IP literals
  through the SOCKS5 domain type, and `CONNECT 1.2.3.4:443` arrives as text.
  `Router::decide` parses such hosts and applies `ip`/`geoip` rules; otherwise an
  address rule is bypassed by how the client encoded its target.
- **Blocked is answered, not dropped**: SOCKS5 reply 2, or HTTP `403 Forbidden`
  naming the rule. A silent drop is an endless spinner.
- **Absolute-form HTTP requests are forwarded with `Connection: close`**, with
  `Proxy-*`, `Connection` and `Keep-Alive` stripped: the relay has dialled one
  host, and a kept-alive client could send its next request, for another host, to
  the first one.
- SOCKS5 address types are 1/3/4; VLESS uses 1/2/3 for the same three things.

**The profile and the work dir**

- **The profile schema is a compatibility surface.** The structs are
  `#[serde(deny_unknown_fields)]`, so renaming or removing a key breaks every
  existing `bliss.toml`. Add keys with `#[serde(default)]`; never rename.
  `bliss.example.toml` is compiled in as the text of every new profile, and
  `links.rs` asserts that it parses and that its documented defaults match the
  code — change them together. It must not contain an active `[routing]`
  header, or the starting rules could never be added to a profile the window
  created (`the_template_is_an_empty_profile_with_routing_left_open`).
- **Every write to the profile goes through `profile_file.rs`**, and each one
  keeps three promises: the file is created on first use, never on launch; new
  text is appended, because a serde round-trip deletes every comment the user
  relies on; and the whole new text is parsed before one `rename` replaces the
  file, so a refusal leaves it byte for byte as it was. After serde fills in
  defaults, a missing section and an empty one look identical; when that
  matters, read the file text (`has_routing_section`).
- **A link is a credential in the window too.** `Command::AddServer` carries a
  `Link` whose `Debug` hides it; the log gets only the server's name; a refused
  link is explained by `LinkError` variant, never by echoing a value — `BadValue`
  holds the rejected text, which can be most of a key. A TOML error shown on
  screen is `ProfileError::brief`, because the full error quotes the offending
  line, and in a profile that line can be a link.
- **Assume another bliss shares the work dir** — a test run, a stray copy, the
  user's own instance. `bliss.log` is appended, and trimmed only if an open with
  `share_mode(0)` succeeds, because Rust opens files with `FILE_SHARE_DELETE` and
  deleting a log another process writes to succeeds silently. The system-proxy
  snapshot records its owner's pid and is not restored while that process lives.
- **On Windows `std::fs::rename` replaces an existing file**
  (`MoveFileExW` + `MOVEFILE_REPLACE_EXISTING`); never `remove_file` first.
  Replacing a running `xray.exe` fails with a sharing violation, as intended.
- **Changing rules or server restarts the tunnel**: inbounds capture the routing
  table when they start. `parts.stop().await` must finish before `start`, or the
  new xray finds the bridge port taken and fails for a reason unrelated to the
  server.

**Style**

- Libraries return `thiserror` errors; the binary renders them with `color-eyre`.
- UI text is Russian with real plural agreement: use `plural` in `ui.rs` (and
  `clipboard::lines` for line counts).

## 6. Dioxus / WebView2 rules (read before touching `ui.rs` or `app.css`)

Dioxus does not simply write the `style` attribute. It records the node's inline
properties, writes the new string, then **restores every property whose value
now reads back empty** (`dioxus-interpreter-js` 0.7.10, `setAttributeInner`, case
`"style"`). Two consequences:

1. **State lives in classes, never in inline style.** An inline property present
   in one state and absent in another is restored and stays forever: the rail
   highlight that "stuck on every item ever clicked", reported three times.
   `NavItem`'s inline style is one constant string; the look per state is
   `.nav-item`, `.nav-item.active`, `.nav-item.disabled` in `app.css`. Guarded by
   `the_rail_item_style_does_not_vary_by_state`.
2. **No `var()` inside a shorthand whose longhands cannot resolve.** Such a value
   is a pending substitution, its longhands read back empty, and the restore loop
   writes the empties over it. Measured on WebView2 by applying each form
   repeatedly through a replica of that function:
   - destroyed on rewrite: `border-radius`, `background`, and the `border` /
     `border-top|right|bottom|left` shorthands;
   - stable: `background-color`, `box-shadow`, `color`, `font-family`, the `font`
     shorthand, and the `border-*-width|style|color` longhands (`ui.rs` uses
     `border-*-color:var(--color-divider)` on purpose).

   Inline radii are therefore literals. 4px, 8px and 14px mirror `--radius-sm`,
   `--radius-md` and `--radius-lg`; 11px, 3px, `1px 1px 0 0` and 50% have no
   token. `radius_literals_match_the_design_tokens` asserts `nocturne.css` still
   declares those three values, and scans all of `ui.rs` except `//` comment
   lines, whitespace-insensitive, for a `var(` inside any of the unstable
   shorthands. Comments are skipped because prose about the rule once tripped it.
3. **The style guards read `ui.rs` as text** (`include_str!("ui.rs")`).
   `the_rail_item_style_does_not_vary_by_state` cuts the source between
   `fn NavItem(` and the doc line `/// Icons follow the label's colour`. Renaming
   `NavItem` or rewording that comment makes it panic in `expect`: fix the
   anchor, never delete the guard.
4. **Hover rules are scoped away from the marked state** (`:not(.active):hover`),
   or the tints stack and look stuck.
5. **Renderer subset.** Blitz has no `overflow:auto`: scrolling panes use
   `overflow-y:scroll;overflow-x:hidden`, panes that fit use `hidden`. No
   `color-mix` or `text-overflow` in `ui.rs`/`app.css` (elide in code); SVG colour
   goes in an attribute. The spike also found Blitz ignores
   `text-transform: uppercase`, does not paint blurred `box-shadow`, does not size
   flex columns to content, and drops whitespace before a nested span. The code
   already relies on some of these (`text-transform:uppercase` inline; `color-mix`
   in `nocturne.css`), so a Blitz build is not free today. Do not add more.
6. A text input fed from the frame (republished twice a second) fights the user
   typing; keep the typed text in a local signal.
7. The window never touches tokio. The engine thread publishes a `Frame` on a
   watch channel; the UI sends `Command`s through `Controller`.
8. **Pictures ship inside the binary, in the stylesheet.** `STYLES` in `ui.rs`
   replaces `url(status-card.jpg)` in `app.css` with a base64 data URI, once. That
   url() must appear in `app.css` exactly once, comments included — a comment
   naming it was replaced too, and tripped `the_scene_matches_the_card_surface`.
   Keep pictures out of inline styles (rule 1).
9. **The icon.** `crates/app/build.rs` compiles `bliss.rc` into the executable
   with `embed-resource`, using `rc.exe` from the Windows SDK. The call is
   `manifest_required`: on MSVC a missing rc.exe fails the build either way,
   but on a GNU toolchain without windres `manifest_optional` would ship an exe
   with no icon. The resource reaches the test binary too, which
   `the_icon_is_compiled_in` relies on. Dioxus shows its own logo on any window
   built without an icon, so `launch` sets one; `set_icons` picks frames for the
   monitor's scale when the window is built, and `WindowIcons` again whenever
   the scale changes. `assets/bliss.ico` has fifteen frames: 16–96 px are the
   mark alone, recentred on the plate with the wordmark painted out, and cover
   both window-icon sizes at every scale Windows offers from 100% to 300% (a
   custom 275% rounds down to the next frame); 128 and 256 px keep the
   wordmark, a smudge at anything smaller, and `icon_side` never asks for them.
   Replace the file with all fifteen
   (`the_icon_has_a_frame_for_every_window_icon_size`). `LoadImageW` resizes
   the nearest frame when there is no exact one, so a test that merely loads
   the icon at some size cannot tell which frames exist.
10. **The status card's scene** (`assets/status-card.jpg`) is the right-hand part
    of a generated night picture: its flat sky (#141423) shifted to
    `--color-surface`, faded to exactly that colour along the left and top
    edges, saved as JPEG q90 without chroma subsampling. A new surface colour
    means remaking the file, or its edge shows as a rectangle. The generated
    originals are not in git.

## 7. Verifying a UI change: look at the real window

A replica page proves a mechanism, not that the code changed. For any UI bug,
drive the real window:

```powershell
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = '--remote-debugging-port=9222'
Start-Process .\target\debug\bliss.exe
# DevTools endpoint: http://127.0.0.1:9222/json  (page title "Dioxus app")
```

**Point it at a scratch profile**, never the real one, whenever the check
writes: `BLISS_PROFILE` replaces `%APPDATA%\bliss\bliss.toml` for the window and
the CLI alike. Leave it unset to test the real first run, and note what it does
not move: the work dir (engine, `xray.json`, `bliss.log`) stays where it is. A
scratch profile whose `[engine] xray` names a file that does not exist makes
any start fail before an engine is spawned or its config written.

From Python with `websocket-client` (installed), after removing the proxy
variables and with `suppress_origin=True`: `Runtime.evaluate` to read classes,
inline styles and `getComputedStyle`; `Input.dispatchMouseEvent` for real clicks
and hover; `Page.captureScreenshot` for images. Record state after every step and
once with the pointer moved away. Close the window afterwards. It still opens
when 10808 is taken, which is fine for UI work.

Two more probes that worked:

- **Window icons:** `WM_GETICON` on the `bliss` window, then `GetIconInfo` and
  `GetDIBits`; compare the pixels with the frame of `assets/bliss.ico` at that
  size. A zero difference proves the resource is in use and the Dioxus logo is
  not.
- **Static replica for a design choice** (not a verification): the Browser pane
  may not paint while Claude's window is behind others. Headless Edge takes the
  picture without stealing focus:
  `msedge.exe --headless=new --no-proxy-server --user-data-dir=<scratch dir> --window-size=W,H --screenshot=<png> <url>`.
  `nocturne.css` has its own `.card` class, so name replica classes differently.

## 8. Debugging discipline

- **Three alternating runs minimum** before concluding something works or does
  not. A flaky server once produced two confident, wrong conclusions in a row.
- **Grep that the edit is in the file** before saying it is fixed.
- **Run a new guard test against the old code** (`git show HEAD:<file>`) and see
  it fail. A guard that cannot fail is decoration.
- **When a fix breaks an old test, ask what that test asserted** — one here had
  encoded the very bug being fixed.
- **Read the dependency's source when behaviour is surprising.** The Dioxus style
  quirk was found in
  `~/.cargo/registry/src/*/dioxus-interpreter-js-0.7.10/src/ts/set_attribute.ts`.

## 9. xray-core facts

- Nearly every release is flagged prerelease, so "latest" can be months old. The
  downloader installs the newest **stable** release; `--newest` takes any.
- Checksums in `.dgst` are labelled `SHA2-256=`.
- Xray now calls `streamSettings.network` `raw` where it used to be `tcp`
  (`config.rs` emits `raw`, `server.rs` accepts both); the `mixed` inbound serves
  SOCKS and HTTP on one port.
- `kill_on_drop` does not survive `TerminateProcess` of the parent; the engine
  runs in a Job Object with kill-on-close (`job.rs`).
- "Ready" means three things (`process.rs`): the bridge port was free before
  spawn, the child is still alive, and whatever answers speaks SOCKS5.
- VLESS Encryption (`encryption=mlkem768x25519plus…`) needs a recent engine:
  `minimum_xray_version` (`config.rs`) says which (25.0), and `check_version`
  (`process.rs`) enforces it in `engine::start` before spawning — major version
  only, so any 25.x passes.
- **xray answers a SOCKS CONNECT as soon as it accepts it, not when the server is
  reached.** Timing `Outbound::connect_tcp` measured 0 ms (`measure_latency` in
  `engine.rs`): a successful dial proves nothing about the tunnel, and an
  unreachable server shows up later as EOF on a stream that looked connected. The
  doc comment on `DIAL_TIMEOUT` in `outbound.rs` claims it bounds the engine's
  connection to the server, which contradicts that. Settle it with three
  alternating runs against a real server before building timeouts, retries or
  health checks on either assumption. Latency is two `HEAD` requests on one
  kept-alive connection, timing the second; the first pays for the handshakes.

## 10. Where things stand

**Done in stage 1:** renderer spike; profile and link parsing; xray management
and download; relay and connection table; system proxy (three modes, crash-safe
restore); routing (proxy / direct / block by domain, address, port, process,
geosite/geoip) and its screen; server list with ping; copy to clipboard; the
application icon and the status card's picture; starting with no profile, no
server and no engine, and adding a server from the window. The
tunnel already starts with the window (`Setup.autostart`).

**Open:** subscriptions (1.6); UDP ASSOCIATE; an idle timeout for half-open
connections; tray and start-with-Windows via Task Scheduler (1.8); the 12-hour
memory run; the console window that appears next to the GUI (the fix needs
testing from Explorer — an earlier attempt broke double-click launch).

**The resource budget is an acceptance criterion; a regression blocks like a
failing test:** GUI idle RSS ≤ 400 MB (the spike measured 353 MB and 7 processes
on WebView2), idle CPU ≤ 1% with the tunnel up, RSS growth ≤ 5% over a 12-hour
run under load. Pushing tens of thousands of entries through the log and the
connection table must not move RSS at all. `mimalloc` is a default feature; take
the 12-hour run twice, with it and with `--no-default-features`, because mimalloc
trades RSS for speed and RSS is the criterion.

**Stage 3 (native stack) — set up before writing code.** A REALITY
authentication failure is silent: the server just serves the real site. Stand up
a private Xray server with `show: true` and `loglevel: debug` first. Use the
RFC 8448 traces as test vectors for the TLS 1.3 key schedule before any live
connection. `shoes` (github.com/cfal/shoes, MIT) implements REALITY on its own
TLS 1.3; compare intermediate values against it (authKey, the `session_id` seal,
the certificate HMAC). The byte-level spec is in the plan's section «Протокольные
детали для этапа 3» — `session_id` at offset 39 of the marshalled ClientHello,
VLESS field order port → atyp → address, VLESS Encryption and Vision framing.
Follow it byte for byte, not from memory.
