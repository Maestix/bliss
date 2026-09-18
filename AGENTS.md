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
  `sysproxy::recover`, creates the work dir, and appends to `bliss.log`. None of
  them writes the profile: only the window does, after a change made in it.
  `recover` acts on a `system-proxy-backup.txt` left by a run that died: it
  writes the old settings back only while the machine still points where bliss
  pointed it, and otherwise deletes the file and leaves the setting alone.
  Before starting any bliss, look at that file and at `ProxyServer` under
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings`, so you
  know which of the two will happen. On September 14 the user's bliss had died
  with the proxy set and v2rayN had since taken the proxy over; the probe window
  started after that deleted the file and changed nothing.
- **Talk to the user in Russian.** Code, comments, docs and commit messages are
  in English.

## 1. Layout

| crate | package | what lives there |
|---|---|---|
| `crates/proto` | `bliss-proto` | pure data: `vless://` parsing (`server.rs`), TOML profile (`profile.rs`), routing rules as written (`routing.rs`), VLESS Encryption parameter (`encryption.rs`), credential redaction (`redact.rs`) |
| `crates/core` | `bliss-core` | engine-agnostic: inbound — SOCKS5 and HTTP on one port (`inbound.rs`, `socks5.rs`, `http.rs`), `Outbound` trait, relay, connection tracker, routing matcher (`routing.rs`), geo `.dat` decoder (`geo.rs`), system proxy (`sysproxy.rs`), port→process lookup (`process.rs`) |
| `crates/xray` | `bliss-xray` | xray config generation, process supervision in a Job Object, GitHub downloader |
| `crates/app` | `bliss` (binary) | CLI (`main.rs`), engine thread (`engine.rs`), Dioxus window (`ui.rs`), the profile and every change the window makes to it (`profile_file.rs`), rule conditions as typed (`rule_text.rs`), subscriptions (`subscription.rs`), starting with Windows (`startup.rs`), log ring buffer (`logs.rs`), clipboard |
| `crates/spike` | `bliss-spike` | renderer measurement (WebView2 vs Blitz), not in `default-members`; build with `cargo build -p bliss-spike --profile spike`; results in `docs/spike-1.0-renderer.md` |

**The seam:** `bliss-core` and `bliss-proto` never depend on `bliss-xray`; only
the `bliss` binary (`main.rs`, `engine.rs`) calls into it. Existing xray
knowledge outside `bliss-xray`, not to be extended: the `[engine] xray` path in
`profile.rs`, Xray's `tcp`/`raw` alias in `server.rs`, and the decoder for Xray's
`.dat` files in `geo.rs`.

Runtime paths on Windows:

- profile: `%APPDATA%\bliss\bliss.toml`, written by bliss itself, whole,
  after every change made in the window, and never on launch; bliss runs
  without it
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

- One test round-trips text through the **real Windows clipboard**, replacing
  whatever the user had copied: run `cargo test -- --skip
  text_survives_the_round_trip` while they may be working. The xray readiness
  tests bind loopback ports.
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
bliss --minimized                             the same, minimized: how Windows starts it at sign-in
bliss check [PATH] | check --link             validate a profile, or one link from stdin
bliss engine list [--limit N]
bliss engine install [--version VERSION] [--newest] [--via ADDR]
bliss route <TARGET> [PATH] [--process NAME]  which rule decides; no network
bliss run [PROFILE] [--server TAG] [--engine-log ENGINE_LOG]
```

`bliss.exe` is a **windows-subsystem** program: a double-click opens the window
and no console beside it. A command joins the console of the terminal it came
from (`attach_console` in `main.rs`); `--minimized` alone is the window as
Windows starts it at sign-in, and gets no console (`wants_console`). Git Bash, and anything
capturing the output, hands over pipes and works as it always did; cmd and
PowerShell do not wait for a windows-subsystem program, so their prompt comes
back before the output does, and `$out = bliss …` captures nothing. In
PowerShell a pipe makes it wait and capture: `bliss check | Out-String`. The first attempt at losing the console went the
other way — a console program calling `FreeConsole` — and started from Explorer
it served traffic with no window at all.

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
  connections (512, `tracker.rs`), HTTP request heads (16 KB, `http.rs`), a
  subscription's body (4 MB, read up to it and no further) and the servers one
  list brings (2 000, `subscription.rs`), servers in a profile however they
  arrive (5 000, `MAX_PROFILE_SERVERS`). The project exists because another
  client's GUI reached 7.8 GB this way.
- **A cap checked after the allocation is not a cap.** The HTTP head budget goes
  into `take` before reading, because `read_line` allocates the whole line first.
- **Declared is not implemented.** An error variant with no constructor, a
  supervision method nobody calls, a guard that cannot fail, a dependency with a
  purpose in a comment and no user — each has shipped here looking like
  protection and providing none. The last one standing was `arc-swap` in the
  workspace `Cargo.toml`, "to swap the routing table under live traffic", which
  no crate used while nothing swapped. The swap exists now, as a lock
  (`Serving::set_router`), and the dependency is gone.
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
  address; sites by country need `geosite`. The user found that out from
  Russian sites still going through the tunnel under `geoip:ru`, so the table
  says it when it is built — a warning for a country whose rule lacks its
  geosite category, naming the category when geosite.dat has one — and a check
  of a name says which rules with a country it went past (`Router::geoip_note`,
  `a_country_rule_says_it_cannot_see_sites`).

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
  `bliss.example.toml` documents the format, for `bliss run <profile>`, and
  `links.rs` asserts that it parses and that its documented defaults match the
  code — change them together.
- **The profile belongs to bliss, the way v2rayN's config belongs to v2rayN.**
  Every change is made in the window and written by `profile_file::Store::save`:
  the whole profile, serialised beside the file and renamed over it, after
  bliss's `HEADER`. Nothing is written on launch. Before the first write in a
  run, a file without that header, or one that does not parse, is copied to
  `bliss.toml.bak` (`.1.bak` and so on if that is taken) — that is how a
  hand-made profile's comments survive. The engine changes a clone of the
  profile and takes it only when the save worked (`save_profile`), so the
  window and the file cannot drift apart. Never write the profile any other way.
- **A link is a credential in the window too.** `Command::Import` carries a
  `Link` whose `Debug` hides it; the log gets only the server's name; a refused
  link is explained by `LinkError` variant, never by echoing a value — `BadValue`
  holds the rejected text, which can be most of a key. A TOML error shown on
  screen is `ProfileError::brief`, because the full error quotes the offending
  line, and in a profile that line can be a link.
- **So is a subscription's address**: its token is in the path. `Subscription`'s
  `Debug` and the window show `shown_url` (scheme, host, port), and the log
  names a subscription by its name. reqwest writes the URL into its own error
  messages, so a failed fetch is described by kind after `without_url` and never
  shown as reqwest wrote it (`subscription::describe`). A finished fetch goes
  back to the engine loop as `FetchDone`, whose `Debug` shows the id alone.
- **Assume another bliss shares the work dir** — a test run, a stray copy, the
  user's own instance. `bliss.log` is appended, and trimmed only if an open with
  `share_mode(0)` succeeds, because Rust opens files with `FILE_SHARE_DELETE` and
  deleting a log another process writes to succeeds silently. The system-proxy
  snapshot records its owner's pid, and while that process lives nobody else
  restores it or overwrites it (`set` returns `ProxyError::OwnedElsewhere`). The
  check is in `restore`, which every path goes through — the tunnel stopping,
  the window closing, a mode applied at startup. It used to be in `recover`
  alone, so a second window opened next to a serving copy pulled that copy's
  proxy out from under it as it started; found while about to open a test
  window next to the user's running bliss (`a_running_copys_settings_are_left_alone`).
- **On Windows `std::fs::rename` replaces an existing file**
  (`MoveFileExW` + `MOVEFILE_REPLACE_EXISTING`); never `remove_file` first.
  Replacing a running `xray.exe` fails with a sharing violation, as intended.
- **Changing a server restarts the tunnel; changing rules does not.** A rule
  change goes through `commit_routing`: save the profile, compile the table, hand
  it to `Serving::set_router`. The next connection is decided by it and those
  already open keep their route. It used to restart the tunnel, which dropped
  every connection for the sake of one rule. A restart for a server must let
  `parts.stop().await` finish before `start`, or the new xray finds the bridge
  port taken and fails for a reason unrelated to the server.
- **Slow work leaves the engine loop and comes back through it.** A
  subscription fetch runs in a task of its own and returns as
  `Command::Fetched` through a *weak* sender: a strong one, held by the loop
  itself, would keep the command channel open forever, and the loop would never
  see the window go.

**Starting with Windows**

- **The entry is a Run value, not a scheduled task** (`startup.rs`): `bliss`
  under HKCU `Software\Microsoft\Windows\CurrentVersion\Run`, the executable's
  path in quotes, then `--minimized` when the window should open minimized. The
  plan named Task Scheduler; the window needs no elevation, the value needs no
  rights, and the Run key is the list Task Manager shows. The settings screen
  writes it on a click, never on launch.
- **Task Manager's switch is another value**: `bliss` under
  `Explorer\StartupApproved\Run`, whose first byte is 2 while the entry is on
  and 3, followed by a timestamp, once the user switched it off (6 and 7 occur
  too, so the test is the low bit). An entry switched off there never starts,
  so bliss reads it as off and says where; ticking the box deletes the switch
  along with writing the entry.
- **The entry names one executable.** One that starts another copy — a build
  moved or copied since — is named on the screen beside «Запускать эту», and
  is rewritten only by that button or the checkbox.
- `BLISS_STARTUP_KEY` moves both keys under a scratch key in HKCU, for probes
  (§7). `the_entry_round_trips_through_the_registry` uses a key of its own one
  level under `Software` and deletes it; nested two levels deep, it left the
  empty parent behind.

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
   token. `radius_literals_match_the_design_tokens` asserts `theme.css` still
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
   in `theme.css`), so a Blitz build is not free today. Do not add more.
6. A text input fed from the frame (republished twice a second) fights the user
   typing; keep the typed text in a local signal.
7. The window never touches tokio. The engine thread publishes a `Frame` on a
   watch channel; the UI sends `Command`s through `Controller`.
8. **Pictures and the font ship inside the binary, in the stylesheets.** `STYLES`
   in `ui.rs` replaces `url(status-card.jpg)` and `url(rail-cat.jpg)` in
   `app.css`, and `THEME_STYLES`
   replaces `url(comfortaa.woff2)` in `theme.css`, with base64 data URIs, once.
   Each url() must appear in its file exactly once, comments included — a
   comment naming the scene was replaced too, and tripped
   `the_scene_matches_the_card_surface`. Keep pictures out of inline styles
   (rule 1). Nothing is fetched from the network: the window opens before the
   tunnel, for people whose network may block font CDNs.
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
    `--color-surface`, everything else nudged toward pink (hue +14 of 255,
    saturation ×1.25), faded to exactly the card colour along the left and top
    edges, saved as JPEG q90 without chroma subsampling. A new surface colour
    means remaking the file, or its edge shows as a rectangle;
    `the_scene_matches_the_card_surface` checks the edge pixels. The generated
    originals are not in git.

    The rail's cat (`assets/rail-cat.jpg`) is a GPT picture on a transparent
    ground whose faint glow reaches almost to its edges: cropped to where that
    glow is at least 8/255 opaque plus 60px, the glow faded to nothing over the
    last 48px of each edge, composited onto `--color-surface`, 400px tall, JPEG
    q90 without chroma subsampling. `the_rail_cat_matches_the_card_surface`
    checks all four edges. `.rail-cat` takes the room the navigation leaves, up
    to 170px, and shrinks first in a short window.
11. **The theme is `theme.css`**: a calm neon — plum grounds, a soft pink accent,
    violet for what is on its way, mint for "fine" and "direct", a soft red for
    failures. It replaced Nocturne, whose token names it kept. Colours that
    cannot be a `var()` — SVG attributes, state tones, `rgba()` tints — are
    literals in `ui.rs` (`ACCENT`, `MUTED`, `GOOD`, `BAD`, `ON`, `BUSY`, `IDLE`,
    `FAILED`, and tints of the accent `rgba(238,130,196,…)`) and `app.css`;
    they mirror the tokens, so change both together. Connected and "through the
    proxy" are both pink, which is why "direct" must never borrow `ON`.

    The font is Comfortaa: text at 600, headings at 700, its top weight. The
    inline `font-weight:700` and `font:700` in `ui.rs` mirror
    `--font-heading-weight`. `assets/comfortaa.woff2` is Google Fonts'
    variable `Comfortaa[wght].ttf` cut with `pyftsubset` to U+0020–007E,
    00A0–00FF, 0400–045F, 0490–0491, 2010–2027, 2030–203A, 2116, 2190–2195
    and 2212, with `--layout-features=*` and `--flavor=woff2`; a character
    outside that set falls back to the system font, so widen the set when the
    window starts drawing new symbols. `OFL-Comfortaa.txt` must stay beside it
    (`the_font_is_inlined_and_licensed`). Its Cyrillic uses Latin-like shapes
    (д like g, т like m); the user chose it over Nunito, whose Cyrillic is
    conventional, after seeing both in the window.
12. **An inline property outranks every class rule, `:hover` and `:focus`
    included.** The action cards carried their resting tint inline, so hovering
    them changed nothing, and an inline `cursor:default` kept the arrow over
    them; the text boxes carried their hairline inline, so `.field:focus` never
    showed. A look that a state changes lives in a class (`.act-card`, `.field`,
    `.search`), and clickable things get `cursor: pointer` from `app.css`:
    `only_the_window_chrome_sets_a_cursor_inline` allows the two inline cursors
    of the title bar and its buttons, and nothing else.
13. **rsx builds handlers in its own order, not the order they are written.** A
    closure cloned inside one handler's block can be cloned after another
    handler has already moved it (E0382). Clone before `rsx!` (`save_on_key` in
    `RuleEditor`, the clones in `ImportForm`).
14. **Keys that work anywhere are heard by the page itself.** A key goes to the
    focused element, and with nothing focused that is the document, outside
    every element rsx can attach a handler to. `KEY_LISTENER`, installed once
    through `document::eval`, sends `paste` (Ctrl+V or Shift+Insert, matched by
    `code` so a Russian layout works) and `escape` back to `App`; it leaves text
    boxes alone and does nothing while a `[data-modal]` element is open. Blitz
    runs no script, so a Blitz build would need another way. What Esc closes
    lives in global signals (`ROW_MENU`, `RULE_MENU`, `DRAFT`), cleared in
    `use_drop` by the screen that owns them, or they reopen on its next visit.
15. **A dialog is never taller than the window.** `.dialog-card` has a
    `max-height` and only `.dialog-body` scrolls. The first rule editor grew
    with its hints, and in the default window the geoip note pushed Save and
    Cancel below the edge: the probe's click there reached nothing, and a
    person would have had no way to save.
16. **The page grows with the window past the default size.** `WindowZoom`
    sets WebView2's zoom (`set_zoom_level`) from `zoom_for` on every resize:
    half of how far the smaller of width/1040 and height/660 is past 1, in 5%
    steps, at most 1.6. Layouts still adapt inside that (the action cards are an
    `auto-fill` grid). CSS pixels are then larger than logical ones, so
    `viewport()` gives the page's own size for placing menus. In DevTools the
    zoom shows as `devicePixelRatio`.
17. **A component is re-run by its parent only when its props change.** One
    with no props is drawn once, and after that only by its own signals.
    `Styles` relies on it. `LogStrip`, which reads the log buffer itself rather
    than through the frame, went stale because of it: the "live" log changed
    only when a click on it set its copy note, and the user found it that way.
    It now takes `logs::revision()` as a prop. Anything that reads shared state
    outside the frame needs a prop that changes with that state, or it goes
    stale without a sound.
18. **A column of text never sizes a table.** A bare `fr` track is at least as
    wide as its longest unbreakable word, a host name has no break in it, and
    each row of a table here is a grid of its own: in a narrow window one long
    name moved its own row's columns and no other's. Text tracks are
    `minmax(0,…fr)`, their cells `min-width:0;overflow:hidden;white-space:nowrap`
    (`CONNECTION_COLUMNS`, `SERVER_COLUMNS`, guarded by
    `table_columns_do_not_grow_with_their_text`). A header over a list that
    scrolls takes `.list-head`: the list's scrollbar narrows the rows alone, by
    9px, and the header's columns stood that far to the right of theirs. The
    class gives the header a scrollbar of the same width with nothing to
    scroll, which draws nothing (rule 21). The subscriptions screen had the same gap between
    its form and its cards, and puts the form inside the scrolling column.
19. **A line that must keep its height hides what does not fit instead of
    wrapping it in place.** `TabBar` is `height:30px;overflow:hidden;
    flex-wrap:wrap`: a hint too wide for the room beside the tabs wraps onto a
    second line, out of sight. It used to wrap within itself in any window under
    1000px wide; the bar grew under the servers tab alone, and the tabs and the
    card below them moved 10px on every switch. What sits in such a line keeps
    one width, or it blinks out and back as its numbers change: the traffic
    tab's line lost "последние 20 с", which the axis under the chart says
    anyway. `flex-wrap` breaks between any two items, so parts that must stay
    side by side are one item: a rule's number and its text are, since as two
    the number took a line of its own.
20. **A window that opens minimized is minimized in its first render**, not
    when it is built. WebView2 cannot be created in a minimized window:
    minimized in `with_on_window`, which runs before the webview exists, bliss
    died at every `--minimized` start with `E_INVALIDARG`, unwrapped inside
    dioxus-desktop, before its window ever appeared
    (`the_window_is_not_minimized_before_its_page_exists`). `App` minimizes it
    in a `use_hook` (`StartMinimized`). dioxus-desktop renders for the first time
    once the page has loaded, while the window is still hidden, and shows it
    right after that render, which leaves a minimized window minimized, with
    its taskbar button: sampled from the moment the process started, the
    window was never on screen. Through tao's `set_minimized`, not
    `ShowWindow`: tao keeps its own copy of the window's state and ends every
    change to it with `SW_HIDE` unless it believes the window visible.
21. **Scrollbars are a thumb drawn by `::-webkit-scrollbar` in `app.css`, and
    nothing sets `scrollbar-width` or `scrollbar-color`.** On an element with
    either, WebView2 ignores the `::-webkit-scrollbar` rules and draws its own
    bar, which keeps its arrows when there is nothing to scroll. Both were set
    on `*`: every pane that fit showed two faint arrows with no bar between
    them — the user found them beside the settings screen's lone card — and
    the `::-webkit-scrollbar` rules had never applied. Without them, measured
    on every screen at zoom 1 and 1.5: nothing is drawn beside a pane that
    fits, a list that scrolls keeps a thumb of at least 28px, every bar is 9px
    wide at both zooms (WebView2's thin bar was 10px and 7px, sized in device
    pixels), and table headers still line up with their rows
    (`the_standard_scrollbar_properties_stay_unset`).

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

**The user may be running bliss while you check.** Look for a `bliss.exe` first
(`Get-CimInstance Win32_Process`, which shows the path). A second copy shares
`%LOCALAPPDATA%\bliss` with the first twice over: the work dir, and WebView2's
data folder, which dioxus-desktop names after the executable
(`%LOCALAPPDATA%\<exe stem>`, `webview.rs`), so moving the binary does not
separate them. Two copies with different WebView2 arguments in one data folder
do not start cleanly. To check next to a running copy, run a copy of the debug
build under another name (`bliss-probe.exe`), or ask the user to close theirs.

From Python with `websocket-client` (installed), after removing the proxy
variables and with `suppress_origin=True`: `Runtime.evaluate` to read classes,
inline styles and `getComputedStyle`; `Input.dispatchMouseEvent` for real clicks,
right clicks (`button: "right"`) and hover; `Input.dispatchKeyEvent` for keys
(Ctrl+V is `code: "KeyV"` with `modifiers: 2`); `Input.insertText` to type into
the focused box, which Dioxus sees as input; `Page.captureScreenshot` for images.
Record state after every step and once with the pointer moved away. Close the
window afterwards. It still opens when 10808 is taken, which is fine for UI work.
Check that a script's page expressions did not throw: `Runtime.evaluate` returns
`exceptionDetails` rather than failing, and a helper that reads only the value
turns every broken selector into a silent `None`.

Give `BLISS_PROFILE` an **absolute** path. The window runs with its own working
directory, and a relative path there names no file: the window opens with no
servers, and every check after that fails for the wrong reason. A check that
needs rows in the connections table has to start the managed engine, on ports
of its own (`[inbound] socks/http`, `[engine] bridge_port`), against a fake
server and a local HTTP server reached through a `direct` rule; that start
writes `xray.json` in the shared work dir, which the next real start rewrites.
Ctrl+V needs the real clipboard: put fake data on it and empty it afterwards.

A narrow window is sized from the script, with `SetWindowPos` from a DPI-aware
process (`SetProcessDpiAwarenessContext(-4)`). The rect includes the window's
invisible resize borders, 16px wide and 9px tall at 96 dpi, and a size set this
way is not held to the window's minimum: ask for 896×669 to get the 880×660 page
a person can reach by dragging. The first try measured a 384px page, which
nobody can see. Something that moves on a switch is caught by recording its
position on every `requestAnimationFrame` across the switch, not once after it.
Pillow is installed, for checking in a screenshot that something meant to be
invisible is: in a scrollbar's gutter beside a pane that fits, count the pixels
off the gutter's commonest colour, at both ends where arrows sat and along it
where a thumb would be; all three are zero now.

**Starting with Windows is probed on a scratch key**, never the real Run key:
`BLISS_STARTUP_KEY=Software\bliss-probe-startup` puts the entry and Task
Manager's switch under it (a binary `bliss` value starting with 3 there plays
Task Manager), and the probe deletes the key when it ends. A start with
`--minimized` is read from the `bliss` window: `IsIconic` and
`IsWindowVisible` both true is minimized with a taskbar button. Sample them in
a tight loop from the moment the process starts, to catch the window showing
itself before it minimizes; `AttachConsole(pid)` failing proves it has no
console. The first minimized start died before its window existed, with
nothing on screen and exit code 101: send the probe copy's stderr to a file
to read a panic, since started from Explorer it has nowhere to print one.

Two more probes that worked:

- **Window icons:** `WM_GETICON` on the `bliss` window, then `GetIconInfo` and
  `GetDIBits`; compare the pixels with the frame of `assets/bliss.ico` at that
  size. A zero difference proves the resource is in use and the Dioxus logo is
  not.
- **Static replica for a design choice** (not a verification): the Browser pane
  may not paint while Claude's window is behind others. Headless Edge takes the
  picture without stealing focus:
  `msedge.exe --headless=new --no-proxy-server --user-data-dir=<scratch dir> --window-size=W,H --screenshot=<png> <url>`.
  `theme.css` has its own `.card` class, so name replica classes differently.

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
application icon, the status card's picture and the rail's cat; starting with no profile, no
server and no engine, and adding a server from the window; the profile written
by bliss itself, with the system-proxy choice remembered; the rule editor (add,
change, reorder, delete, the fallback) with rules applied under live traffic,
and rules from a right-click on a connection; search and sorting in the
connections table and a resizable log; Ctrl+V for links, lists and
subscriptions; subscriptions (1.6: base64 or plain lists, `subscription-userinfo`
and `profile-title`, a refresh every 12 hours, through the tunnel when it is
up); zoom with the window size; no console window; deleting servers, a
subscription's servers grouped under its name, a rule filter for the
connections table, and log levels in colour; a settings screen, which so far
starts bliss with Windows, minimized or not; scrollbars that draw only a thumb.
The tunnel already starts with the window (`Setup.autostart`).

**Open:** renaming servers; the rest of the settings screen (ports, the
engine, installing it with a button); UDP ASSOCIATE; an idle timeout for
half-open connections; the tray (1.8); the 12-hour memory run.

**The resource budget is an acceptance criterion; a regression blocks like a
failing test:** GUI idle RSS ≤ 400 MB (the spike measured 353 MB and 7 processes
on WebView2), idle CPU ≤ 1% with the tunnel up, RSS growth ≤ 5% over a 12-hour
run under load. Pushing tens of thousands of entries through the log and the
connection table must not move RSS at all. `mimalloc` is a default feature; take
the 12-hour run twice, with it and with `--no-default-features`, because mimalloc
trades RSS for speed and RSS is the criterion.

**Before stage 3, look at xray-rust.** `github.com/aimalygin/xray-rust`
(MPL-2.0) was found by the user on September 14 and deliberately set aside
until bliss's own features are finished. It is an embeddable Rust
implementation of the Xray client: VLESS over raw TCP, WebSocket, gRPC and
XHTTP; TLS and REALITY with uTLS-shaped ClientHellos (61 fingerprints, through
the author's own `shaped-rustls` fork pinned by commit); Vision; UDP and XUDP;
and VLESS Encryption `mlkem768x25519plus` in all three modes. `xray_core_rs::Core`
starts from an Xray config and reports its inbound's address, so it could
replace `xray.exe` in-process behind the SOCKS seam bliss already has. Against
it, as of that day: three and a half months old, one maintainer, pre-1.0 by its
own account, CI on Linux only, TUN for Android and Apple only. The spike that
decides it: build it on Windows (rustc 1.96+, CMake and clang are here;
aws-lc-rs's `prebuilt-nasm` covers NASM), run it with the config `bliss-xray`
generates against the user's server, and compare memory and throughput with
xray.exe. Downloading it needs the user's go-ahead.

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
