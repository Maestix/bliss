# AGENTS.md — bliss

Instructions for coding agents working in this repository. Everything here was
learned the expensive way; most entries name the bug that taught it. Every
claim was fact-checked against the code, git and this machine when the file was
written — if you find one that has gone stale, fix the file in the same commit
as the code. Read the whole file before the first edit.

bliss is a proxy client for Windows, written in Rust. It began as a VLESS +
REALITY client and now reads a link for every kind of server xray-core can
reach: VLESS, VMess, Trojan, Shadowsocks, SOCKS, Hysteria 2 and WireGuard, over
every transport xray still has. Stage 1
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
  - any server link — `vless://`, `vmess://`, `trojan://`, `ss://`,
    `socks://`, `hysteria2://`, `wireguard://` — and a subscription's address.

  The last four live outside the repository; never copy them in. To learn the
  xray config schema, read `config.rs`, not a generated file.
- **Test data is fake by construction**: reserved names (`example.net`,
  `a.example`), TEST-NET-3 addresses (`203.0.113.0/24`), private ranges, literals
  such as `1.2.3.4`, the UUID `11111111-2222-…`, and repeated-letter keys and
  passwords (`AAAA…`, `PPPP…`). Keep new fixtures in that style so nobody has
  to wonder whether a random-looking key is real. The one exception is
  `crates/xray/tests/engine.rs`, which needs working key pairs and has xray
  derive them, at run time, from fixed repeated-byte private keys.
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
| `crates/proto` | `bliss-proto` | pure data: the parsed server — protocol, security, transport — and its redacting types (`server.rs`); share links, one parser per scheme in `link/` (`vless.rs`, `vmess.rs`, `trojan.rs`, `shadowsocks.rs`, `socks.rs`, `hysteria2.rs`, `wireguard.rs`) and the transport and TLS parameters they share (`link/stream.rs`); TOML profile (`profile.rs`), routing rules as written (`routing.rs`), VLESS Encryption parameter (`encryption.rs`), credential redaction (`redact.rs`) |
| `crates/core` | `bliss-core` | engine-agnostic: inbound — SOCKS5 and HTTP on one port (`inbound.rs`, `socks5.rs`, `http.rs`), `Outbound` trait, relay, connection tracker, routing matcher (`routing.rs`), geo `.dat` decoder (`geo.rs`), system proxy (`sysproxy.rs`), port→process lookup (`process.rs`) |
| `crates/xray` | `bliss-xray` | xray config generation for every protocol and transport, written for the engine's version (`config.rs`), process supervision in a Job Object, GitHub downloader; `tests/engine.rs` carries every kind of link end to end through a real xray (§2) |
| `crates/tun` | `bliss-tun` | stage 2: `wintun.dll` carried inside the binary (`lib.rs`), the adapter (`device.rs`), the machine's addresses and routes (`net.rs`), and packets turned into connections and flows (`stack.rs`) |
| `crates/app` | `bliss` (binary) | CLI (`main.rs`), engine thread (`engine.rs`), Dioxus window (`ui.rs`), the profile and every change the window makes to it (`profile_file.rs`), rule conditions as typed (`rule_text.rs`), subscriptions (`subscription.rs`), starting with Windows (`startup.rs`), log ring buffer (`logs.rs`), clipboard |
| `crates/spike` | `bliss-spike` | renderer measurement (WebView2 vs Blitz), not in `default-members`; build with `cargo build -p bliss-spike --profile spike`; results in `docs/spike-1.0-renderer.md` |

**The seam:** `bliss-core` and `bliss-proto` never depend on `bliss-xray`; only
the `bliss` binary (`main.rs`, `engine.rs`) calls into it. Existing xray
knowledge outside `bliss-xray`, not to be extended: the `[engine] xray` path in
`profile.rs`, and the decoder for Xray's `.dat` files in `geo.rs`. One kind is
there on purpose: **`link/` knows what xray can carry.** Share links are the
Xray ecosystem's own format, and bliss carries only what its engine does, so a
link xray cannot serve — a transport it removed (`h2`, `quic`), REALITY over
anything but raw, XHTTP or gRPC, a Shadowsocks stream cipher, a protocol it
never had (`tuic://`) — is refused when it is pasted rather than when xray
refuses the config at start, in words the window cannot show. The shape of the
config xray reads stays in `config.rs`. Stage 3's native stack will speak VLESS
alone; everything else stays on xray, so this list does not move then.

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
- **Every link bliss reads is carried end to end by a real xray** in
  `crates/xray/tests/engine.rs`, ignored by default:

  ```bash
  cargo test -p bliss-xray --test engine -- --ignored --nocapture
  ```

  Per case, a server xray configured by hand from Xray's own config code, a
  client xray configured by `bliss_xray::build` from a share link, and a TCP
  and a UDP echo, reached through the client's SOCKS port as bliss reaches it:
  27 cases, every protocol, every transport, TLS pinned by `pcs`, REALITY with
  Vision and VLESS Encryption, mKCP masks, Salamander and port hopping. Keys and
  a certificate are made per run by xray itself (`tls cert`, `tls hash`, `wg -i`,
  `x25519 -i`, `vlessenc`) and deleted after. `BLISS_ENGINE_CASE=<part of a
  name>` runs a subset, `BLISS_ENGINE_DEBUG=1` has both xrays log at debug,
  `BLISS_XRAY` names another executable. Everything stays on 127.0.0.1, which
  on Windows also keeps it out of any TUN adapter another client has up — the
  user ran it next to Throne's. `xray run -test` would not do: a config that
  dials the wrong way passes it. Run this after any change to `config.rs` or
  `link/`. Its first runs found a bug no unit test could — a port hop without
  an interval kills the whole engine — and two ways a hand-made server differs
  from a real one (§10).
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
bliss --minimized                             the same, in the tray: how Windows starts it at sign-in
bliss check [PATH] | check --link             validate a profile, or one link from stdin
bliss engine list [--limit N]
bliss engine install [--version VERSION] [--newest] [--via ADDR]
bliss route <TARGET> [PATH] [--process NAME]  which rule decides; no network
bliss run [PROFILE] [--server TAG] [--engine-log ENGINE_LOG] [--tun]
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
8. **A file put back with `mv` or `cp -p` keeps its old modification time**, and
   cargo decides what to rebuild by it: after breaking a file on purpose to see
   a guard fail and moving the saved copy back, the test binary was still the
   broken build, and the guard went on failing against correct code. `touch`
   the restored file, or restore by writing it.

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
- Design sources (`*.zip`) stay out of git. `crates/tun/assets/wintun.dll` is in
  it on purpose, with its licence beside it: bliss ships as one file, so the
  library is carried inside the binary (`include_bytes!`) rather than downloaded.

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
  `PublicKey`, `VlessEncryption` and `EncryptionKey`, which show both ends of
  the value, and for `Hidden` and `PrivateKey`, which show nothing at all.
  A UUID's ends are safe to show; eight characters of a sixteen-character
  password are most of it, so every password, Hysteria auth, mKCP seed,
  WireGuard private key, and any JSON a link carries (`extra`, `fm` — either
  can hold a password) is `Hidden`. The UUID and public key are guarded by
  `debug_does_not_leak_credentials`, the passwords and keys of every protocol
  by `no_protocol_leaks_its_secret_into_debug`, the encryption key only by the
  `parsed_reality_link_matches_snapshot` snapshot. A refusal never quotes what
  it refuses when that can be secret: a VMess blob that is not JSON is
  "something else", and a Shadowsocks cipher xray dropped is named without the
  password written beside it.
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
  port taken and fails for a reason unrelated to the server. A rule's switch
  and a preset are rule changes like any other: no restart.
- **A rule can be switched off, and presets put the switches back.** Asked for
  by the user with many rules and several ways of using them, so as not to
  flip each one by hand.
  - Off (`enabled = false`, written only when off), a rule keeps its row and
    its number and decides nothing: `Router::build` skips it and does not even
    load its geo categories, its warnings keep the screen's numbering, and the
    TUN note leaves it out (`a_switched_off_rule_decides_nothing`).
  - **A preset names rules by id**, not by place or name, both of which
    change: every switch's position (`on`, `off`) and where everything else
    goes, under a name. Applying one leaves a rule it has never heard of —
    made since — as it is. `current_preset` is the one applied or saved last;
    `drifted` is true once a switch or the fallback has moved since, which the
    window shows as a violet dot and the tick saves into it.
  - Ids come from `Routing::ensure_ids`, called in `commit_routing` before
    every save, so every saved rule has one: one more than any id a rule or a
    preset holds, so a preset that still says where a deleted rule stood is
    never read as the next rule (`an_id_is_never_handed_out_twice`). The
    editor sends only words, so `save_rule` keeps the id and the switch of the
    rule it replaces; `delete_rule` takes the id out of every preset.
  - At most `MAX_PRESETS` (24), with the test that fails without the cap.
- **Slow work leaves the engine loop and comes back through it.** A
  subscription fetch runs in a task of its own and returns as
  `Command::Fetched` through a *weak* sender: a strong one, held by the loop
  itself, would keep the command channel open forever, and the loop would never
  see the window go.

**Starting with Windows**

- **The entry is a Run value, not a scheduled task** (`startup.rs`): `bliss`
  under HKCU `Software\Microsoft\Windows\CurrentVersion\Run`, the executable's
  path in quotes, then `--minimized` when bliss should start in the tray. The
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
   in `ui.rs` replaces `url(status-card.jpg)`, `url(rail-cat.jpg)` and
   `url(tray-bunny.jpg)` in `app.css`, and `THEME_STYLES`
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

    The tray menu's bunny (`assets/tray-bunny.jpg`) is made the same way, 300px
    tall, and sits in the corner of `.tray-card` under a gradient that fades it
    out across the words, as the status card's scene does
    (`the_tray_bunny_matches_the_card_surface`).
11. **The theme is `theme.css`**: a calm neon — plum grounds, a soft pink accent,
    violet for what is on its way, mint for "fine" and "direct", a soft red for
    failures. It replaced Nocturne, whose token names it kept. Colours that
    cannot be a `var()` — SVG attributes, state tones, `rgba()` tints — are
    literals in `ui.rs` (`ACCENT`, `MUTED`, `GOOD`, `BAD`, `ON`, `BUSY`, `IDLE`,
    `FAILED`, and tints of the accent `rgba(238,130,196,…)`) and `app.css`;
    they mirror the tokens, so change both together. Connected and "through the
    proxy" are both pink, which is why "direct" must never borrow `ON`.

    **A mark repeated down a list stays quiet.** The rule switch's first knob
    was solid pink with a glow; nearly every rule is on, so the list grew a
    column of glowing dots louder than the action pills beside them, and the
    user said so. It is a muted pink dot on a thin accent border now
    (`.rule-switch`), picked from four looks drawn with the real stylesheets
    by headless Edge (§7's static replica), and checked pixel for pixel
    against the pick. The ring is the one thing on screen that glows.

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

    **It has no tabular figures**, so every `font-variant-numeric:tabular-nums`
    in `ui.rs` — thirteen of them — has never done anything: the property asks
    for a `tnum` feature the file does not carry. Its digits run 378 to 647
    units of 1000, a spread of 0.269em, and the clock on the status card moved
    the rest of its line by 7.4px between `00:00:11` and `00:00:44`, measured
    in the window at 14px. The user saw it as text hopping about. `Ticking`
    puts each digit in `.figure`, a box as wide as the widest digit and
    centred — which is what a tabular figure is — and the same measurement
    then reads 0. Modifying the font instead was refused on purpose: the OFL
    reserves the name "Comfortaa", so a changed file would have to be renamed,
    and it is a variable font where advances live in `hmtx`, `HVAR` and `gvar`
    at once. `the_numbers_that_tick_stand_still` holds the box's width in
    app.css to the measurement in `FIGURE_EM`.
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
20. **A window that starts in the tray is built hidden, never minimized.**
    WebView2 cannot be created in a minimized window: minimized in
    `with_on_window`, which runs before the webview exists, bliss died at every
    `--minimized` start with `E_INVALIDARG`, unwrapped inside dioxus-desktop,
    before its window ever appeared. `with_visible(false)` costs nothing, since
    dioxus-desktop builds every window hidden anyway and shows it after the
    first render: a window that starts in the tray is one it is never told to
    show. The guard stands
    (`the_window_is_not_minimized_before_its_page_exists`): nothing in `launch`
    may minimize. Move a window through tao, never `ShowWindow` — tao keeps its
    own copy of the window's state and ends every change to it with `SW_HIDE`
    unless it believes the window visible.
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
22. **The traffic chart glides; it does not step.** Forty bars each jumped a
    place twice a second, which the user saw as a chart moving in jerks. It is
    a line through `CHART_SAMPLES` (42) download rates, one past each edge of
    the chart, on a layer 41/40 of the chart wide that a CSS animation moves
    left by one sample, 1/41 of its width, over one `TICK`, in fifteen steps.
    A new sample restarts the glide by switching between two identical
    keyframe names (`glide_class`, by the parity of `Frame.samples`), so the
    data shifts in the render that starts the glide again from zero. Gliding
    at the display's rate, over 900 frames the line never moved back or jumped
    more than 0.25px; in steps it moves 30 times a second, 1.2px at a time,
    and still never back. The numbers in
    `app.css` follow the constants (`the_chart_glides_one_sample_per_tick`).
    - The curve is monotone (Fritsch–Carlson): it never dips below zero beside
      a burst (`the_chart_line_never_overshoots_a_sample`).
    - The paths are in units of one KB/s and the range is a CSS transform on
      the SVG group, with a transition, so a change of range eases.
    - The rate under the pointer is a script, `CHART_HOVER`: the samples glide
      out from under a pointer that holds still, which no rsx handler hears.
      It writes on `.chart` only what rsx does not own there (custom
      properties, `data-hover`). Blitz runs no script and was not seen to
      transform SVG through CSS, so a Blitz build would lose the readout and
      the easing.
    - **The glide costs CPU while it is on screen**: 12–20% of one core with
      traffic flowing, against 3% for the bars, nearly all of it WebView2's GPU
      process and renderer drawing at the display's rate. The user found that
      too much for one chart, so the glide takes fifteen steps a tick, 30
      frames a second: 8% of a core against 12% in the same run. Ten steps
      saved nothing more, since the renderer still wakes at the display's rate
      and only the frames that change get cheaper. It costs nothing where it is
      not seen:
      21 s without traffic and the chart stands still (1.35% of a core, as on
      the logs screen), and a minimized window draws nothing (2.3% with
      traffic flowing), which matters for a bliss that starts minimized.

23. **The tray's menu is this window, in a small shape.** Windows draws its own
    menus in its own font and colours, and the one Dioxus offers a tray icon is
    one of those, so bliss builds its icon without a menu and shows `TrayMenu`
    instead: the same webview, the same stylesheet, one card (`into_menu`,
    `out_of_menu`). No second webview to pay for, and nothing else would look
    like the program.
    - **A native menu also stops everything.** `TrackPopupMenu` runs a message
      loop of its own inside the shell's click, and while it is up the window
      answers no events at all — including the one that would open bliss's own
      menu. A stopgap "Выход" item was enough to make every right click do
      nothing whatsoever.
    - **Shape it out of sight.** Hidden, resized, moved, and only then shown —
      by an effect, after the render that puts the menu on the page. Shown
      first, the window stood for a frame with the whole program squeezed into
      300px; reshaped while still visible on the way back, it flashed the
      program at the centre of the screen, which is what the user saw both when
      the menu closed and when bliss quit.
    - **Hide it at once, never faded.** tao keeps a caption style under the
      frame bliss draws (for the shadow and snapping), so Windows fades the
      window out when it is hidden — and the fade follows the window. Hidden
      and given back the program's place in one breath, the menu flashed at
      that place, the top-left corner for a bliss started in the tray, on
      every click away from it; the user saw it. `out_of_menu` turns the
      window's animations off (`animated`, `DWMWA_TRANSITIONS_FORCEDISABLED`)
      before hiding it and on again once it stands where the program does
      (`the_menu_leaves_without_a_fade`).
    - **Closing on lost focus needs two guards**: the menu goes when the focus
      goes, but only once it has had the focus (`MENU_HELD`) and 250ms after it
      opened. Windows hands the focus around as a window appears, and without
      them the menu opened and vanished inside 50ms.
    - **The menu has one shape.** tao lets a borderless window be dragged by
      its edges while it is resizable (its own `WM_NCHITTEST`), and Win+Up
      maximizes anything with a maximize box, so the user could stretch the
      menu. `into_menu` turns both off and `out_of_menu` back on
      (`the_menu_keeps_its_shape`). It is 460px tall: everything but the
      server list takes about 270px, and the list, the one part that gives,
      shows six servers and the edge of a seventh. Presets make it taller
      rather than taking the list's room: 37px and a 28px line for each, four
      at most, past which `.tray-presets` scrolls (`menu_height`,
      `the_menu_grows_with_its_presets`, which holds the CSS to the same
      numbers). The click handler that opens it is made once, so it reads the
      frame's signal when clicked; a count captured when it was made would be
      the count at launch.
    - The close button and Alt+F4 hide the window
      (`WindowCloseBehaviour::WindowHides`), so bliss goes on serving; leaving
      is "Выход" in the menu, which sets `WindowCloses` first and then closes
      the last window.
    - The program's screen is folded away by class while the tray has the
      window, and `display` lives in `.shell` rather than in the inline style:
      `.shell.folded { display: none }` did nothing until the inline
      `display:flex` moved out of its way (rule 12).
24. **One window per executable, and a second copy leaves at once.** WebView2
    gives one browser process to one user data folder, and dioxus-desktop names
    that folder after the executable — so a second `bliss.exe` **never gets a
    webview and therefore never shows a window at all**, while still taking a
    turn at the profile, the ports and the work dir. Measured on 20 September:
    the second and third starts each sat there with a window handle, no child
    process and no page, while the first went on working untouched. That is
    what the user had been fighting for an evening: they always have one bliss
    running, every copy they started after it was invisible, and the window
    they were clicking belonged to an older build with no switch in it.
    `only_window` in `main.rs` takes a session-local named mutex before the
    window is built and leaves quietly when another copy holds it, raising that
    copy's window if it is on screen (`SetForegroundWindow` only — a window in
    the tray belongs to tao and is not shown from outside, rule 20). The claim
    is named after the executable, like the folder it stands for, so a renamed
    probe copy is a separate claim and may run beside the user's bliss (§7);
    `only_one_copy_holds_the_window` takes a claim of its own rather than
    fighting whatever is running.

25. **An elevated window works; what froze it was a mouse script.** On 20
    September an elevated bliss drew its first frame and then took no click at
    all: not the page, not its own title bar, not even the taskbar while it had
    the focus. What was measured then was real — `bliss.exe` at high
    integrity, its six `msedgewebview2.exe` at medium, the renderer idle at
    0.000s of CPU — and the conclusion drawn from it was wrong: that Windows
    passes nothing between the two levels. For two days `window()` refused to
    open elevated and TUN was sent to a console. The cause was the user's
    AutoHotkey script against a bouncing mouse button, which swallowed every
    real click and sent a synthetic one in its place; Windows does not let a
    medium-integrity program inject input into a high-integrity window, so
    with an elevated window in front the clicks went nowhere at all. The user
    found it: with the script stopped, an elevated copy answered every click.
    The refusal is gone, and the TUN switch and its toast say to restart bliss
    as administrator. What stands:
    - **An idle renderer is the symptom of input that never arrived**, not
      proof that the page cannot receive it.
    - **When nothing on the machine takes clicks, bliss is not the only
      suspect.** Ask what else is running that hooks the mouse or keyboard.
    - The debugging port of §7 did not open in the elevated run, with
      `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` set in a `.cmd` file started
      through `Start-Process -Verb RunAs` (which does not carry the caller's
      environment across). Why was never established.

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

**The user may be running bliss while you check**, and a second copy under the
same name will now refuse to start (rule 24). Look for a `bliss.exe` first
(`Get-CimInstance Win32_Process`, which shows the path). A copy shares
`%LOCALAPPDATA%\bliss` with the first twice over: the work dir, and WebView2's
data folder, which dioxus-desktop names after the executable
(`%LOCALAPPDATA%\<exe stem>`, `webview.rs`), so moving the binary does not
separate them. **To check next to a running copy, run the build under another
name** (`bliss-probe.exe`): that is a data folder of its own and a claim of its
own, and both copies then have a window. Verified on 20 September — one
`bliss.exe` with its webview, a second `bliss.exe` gone within a second, and
`bliss-probe.exe` beside them with a webview of its own.

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

**Motion and CPU.** What animates is read on every `requestAnimationFrame` in
the page: the glide layer's computed transform, with each data shift (a change
of glide class) added back, is one sample's place on screen, which must only
ever move left. CPU is the process tree's time from psutil over 15–30 s, split
by WebView2 process (`--type=` in its command line), with the window kept
above others without activating it (`SetWindowPos`, `HWND_TOPMOST`,
`SWP_NOACTIVATE`), because a window behind others may not draw at all. The same
state measured 12% and 20% of a core in two runs: compare states within one
run. A readout's position is checked against the SVG path's own geometry
(`getPointAtLength` through `getScreenCTM`), not against the script's
arithmetic, which would only agree with itself.

**The tray is driven by the message the shell sends.** The icon's window is
`tray_icon_app` in bliss's own process — `EnumWindows` misses it, being
message-only, so walk `FindWindowExW(HWND_MESSAGE, …)` as well — and
`PostMessage(tray, 6002, 0, WM_LBUTTONUP)`, or `WM_RBUTTONUP`, is exactly what
Windows posts on a click, so the handler runs as it would for a person. Two
things about the menu: it closes when the window loses focus, and a window a
script opened holds none, so call `SetForegroundWindow` on it first or the menu
is gone before the first question; and closing no longer ends bliss, so a probe
kills what it started, while the DevTools socket dies with the "Выход" it just
clicked — expect that rather than reading it as a failure.

**The user's screen is theirs.** Probes open real windows on it, and the tray's
menu pops up wherever the pointer is. Ask before a run when they are at the
machine, and take them at their word when they say not to: work from the code
and the tests instead, and say plainly which claims are then untested.

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

## 9. The TUN adapter (stage 2)

A TUN adapter is another way in, not another program: packets become
connections and flows, and from there they take the path the SOCKS and HTTP
inbounds take — `Router::decide`, the dialer, `relay`, the same connection
table.

- **Asked for in three places, honoured in one.** `[tun] enabled` in the
  profile is the setting; the switch under the ring on the main screen writes
  it (`TunSwitch` -> `Command::Tun`, applied to a live tunnel without
  restarting it — the adapter is another way in, and the connections through
  the two ports have no reason to be dropped for it); and `bliss run --tun`
  turns it on for one run without writing anything. None of that creates an
  adapter without administrator rights: `bliss_tun::elevated()` is asked
  first, and a copy without them serves the two ports as usual. It never fails
  the tunnel — a bliss started from the Run key at sign-in has no rights and
  no way to ask for them, and one that refused to start over that would take
  away more than it explains.
  **A window started as administrator raises the adapter itself** (rule 25:
  the elevated window works). One started without rights saves the choice and
  says to restart bliss as administrator, which then raises it from the saved
  choice. The plan's stage-2 helper or service is what would let an ordinary
  window have TUN without running elevated.
- **Raising the adapter leaves the loop and comes back through it.**
  `Tun::up` is blocking work — loading the library, creating the adapter (a
  driver install the first time), writing routes — and the first version
  called it from the engine loop. The window then answered nothing at all:
  not the switch, not the heart, not the proxy choice, because every command
  waits its turn in that loop. The user reported it as "everything freezes",
  and it is the same rule the subscription fetch already follows. It now runs
  in `spawn_blocking` inside a task, and the finished adapter arrives as
  `Command::TunUp` — which carries the `Serving` it was built for, because a
  restart in between leaves an adapter pointed at a tunnel nobody serves
  (`Arc::ptr_eq` decides). `Running.tun_rising` stops a second click starting
  a second adapter while the first is still installing.
- **A switch that moved and changed nothing has to say why.** The pill's two
  positions are "the ports" and "the whole machine", and the knob moves to
  where the adapter was *asked* to be: `.tun-switch.on.waiting` is the violet
  "on its way" tone for a click that saved the choice and could not act on it.
  The reason goes in a toast at that moment, because a colour and a tooltip
  are not an answer to a click. The three facts are separate in `TunView` —
  asked for, allowed, carrying — since they come apart in every combination.

- **Everything the tunnel needs must go around the tunnel.** Two traps, two
  different answers, and each one is a loop without it.
  - The engine's connection to the server is another process's socket, so it
    is kept out by a route: the server's addresses go `/32` through the old
    default gateway (`Routes::around_tunnel`). The name is resolved on this
    machine to write it, which is the second place bliss resolves anything —
    the latency probe is the first.
  - A connection bliss dials itself for a `direct` rule is this process's
    socket, and a route per destination would be absurd. It is bound to the
    real interface with `IP_UNICAST_IF` (`net::outside`), the one socket
    option that overrides the destination lookup. Without it a direct route
    dials, the packet comes back in through the adapter, is decided `direct`
    again, and dials again for as long as the machine has sockets.
  - **`IP_UNICAST_IF` takes the interface index in network byte order** for
    IPv4 and in host order for IPv6. Swapped, it names an interface that does
    not exist and fails with "the requested address is not valid in its
    context", which says nothing about byte order
    (`the_interface_index_travels_backwards`).
- **The routes are the two halves `0.0.0.0/1` and `128.0.0.0/1`**, not a
  replacement default route: a longer prefix wins on its own, so the machine's
  own default route is never touched and a bliss that dies leaves it where it
  was.
- **Nothing is written down to be undone.** The routes into the adapter die
  with the adapter, and wintun removes the adapter when the process ends,
  however it ends — so unlike the system proxy there is no snapshot on disk
  and no recovery path. `Tun` holds the routes and the device, in that
  declaration order, so dropping it takes the routes away first.
- **A TUN destination is always an address.** The name the program looked up
  is gone by the time a packet exists, so `domain` and `geosite` rules do not
  match TUN traffic at all; `ip`, `geoip`, `port` and `process` do. Answering
  the DNS query and handing out an address bliss can recognise again — fake-IP
  — is what makes the rest of the rules work, and it is not written yet.
  While the adapter is on, the routing screen names the rules this silences
  (`blind_under_tun`): the ones whose only conditions are domains and geosite
  sets. A rule that also has an address, a port or a program still decides
  with those, so it is left out rather than buried in the list. The user's own
  DNS is `dnscrypt-proxy` on loopback, which never enters the adapter, so
  intercepting port 53 would see none of it: for that machine the next step is
  the name out of the connection itself (SNI, `Host:`), not fake-IP.
- **Datagrams have their own port table.** `name_for_port` reads the TCP
  table; a flow's program comes from `name_for_datagram_port`, which reads the
  UDP one. Without it `process:Discord.exe` would match everything Discord
  loads and none of what it says.
- **UDP goes out over SOCKS5 UDP ASSOCIATE** (`Outbound::bind_udp`): a TCP
  connection asks for the association and must be held for its whole life,
  because the proxy drops it when that connection closes. Each datagram
  carries `[RSV RSV FRAG][address][payload]`; a fragment is refused rather
  than passed on as if it were whole. `relay_datagrams` is `relay`'s
  counterpart and differs in two ways that are both UDP's: one datagram per
  read and per write, never split or joined, and the flow ends when either
  direction does, because nothing can say it has finished.
- **The libraries disagree about two things, quietly.** wintun-bindings hands
  out `windows_sys`' `NET_LUID_LH` while IP Helper here is the `windows`
  crate's — the same eight bytes declared twice, so the number crosses and the
  type does not. And its session speaks the *futures* `AsyncRead`/`AsyncWrite`
  while ipstack wants tokio's, which is what `tokio-util`'s `compat` is for.
- **ipstack ends a connection with `block_in_place`**, so it must run on a
  multi-threaded runtime. Both of bliss's are.
- **Still missing, and each is a visible hole:** IPv6 is neither carried nor
  blocked, so a machine with working IPv6 reaches IPv6 sites around the
  tunnel; DNS still goes out the physical interface, which is a leak Windows
  makes worse by asking every adapter at once; and there is no service or
  helper, so TUN needs the whole of bliss running as administrator.

## 10. xray-core facts

- Nearly every release is flagged prerelease, so "latest" can be months old. The
  downloader installs the newest **stable** release; `--newest` takes any.
- Checksums in `.dgst` are labelled `SHA2-256=`.
- Xray now calls `streamSettings.network` `raw` where it used to be `tcp`
  (`config.rs` emits `raw`, `link/stream.rs` accepts both); the `mixed` inbound
  serves SOCKS and HTTP on one port.
- `kill_on_drop` does not survive `TerminateProcess` of the parent; the engine
  runs in a Job Object with kill-on-close (`job.rs`).
- "Ready" means three things (`process.rs`): the bridge port was free before
  spawn, the child is still alive, and whatever answers speaks SOCKS5.
- **A config is written for one engine version.** `check_version` reads it
  (`Version`, compared as numbers: 26.10 is after 26.9) and refuses an engine
  older than `minimum_xray_version` asks for, in `engine::start` before
  spawning; `build` takes it too. VLESS Encryption
  (`encryption=mlkem768x25519plus…`) needs 25.0; Hysteria 2, mKCP and a
  link's own `fm` need 26.2, where finalmask arrived; `pcs` needs 26.1.
- **The schema moves under a link, release by release.** Checked in Xray's
  `infra/conf` at each tag, and what bliss does about it:
  - `allowInsecure` is refused outright after 2026-06-01 — a date check inside
    26.3.27 itself, which fails the whole config. bliss never writes it; a
    certificate is pinned with `pinnedPeerCertSha256` (`pcs` in a link, and
    Hysteria's `pinSHA256`) or checked against other names
    (`verifyPeerCertByName`, `vcn`). A link that asks for insecure with no pin
    gets a warning in the log at start, because a self-signed server will
    then be refused and nothing else would say why.
  - mKCP's `header` and `seed` left `kcpSettings` in 26.2 for finalmask masks,
    innermost first: `mkcp-aes128gcm` with the seed as password (or
    `mkcp-original` without one) under a `header-*` disguise. In 26.6 both
    became `mkcp-legacy`, with `value` and `header`. `kcp_masks` writes
    whichever the engine reads; only the 26.2–26.5 form was ever run (on
    26.3.27), the other is v2rayN's and untested here.
  - HTTP/2 (`h2`, `http`) and QUIC transports are gone; such links are refused
    when pasted. REALITY works over RAW, XHTTP and gRPC only.
  - Hysteria 2 (26.1) is the `hysteria` protocol over the `hysteria`
    transport: the password goes in `hysteriaSettings.auth`, Salamander and
    port hopping in `finalmask`. **A hop needs an interval**: without one
    26.3.27 panics on the first connection ("non-positive interval for
    NewTicker") and takes the engine with it. No link carries one, so it is
    Hysteria's own 30 s. Its ALPN is not ours to set — quic-go's HTTP/3
    transport writes `h3` over any — but a *server* must offer `h3`, or it
    refuses every client with `CRYPTO_ERROR 0x178 … no application protocol`;
    the engine test's first hand-made server did.
  - WireGuard has no `streamSettings`, and xray's WireGuard server hands its
    packets to gVisor, which drops any addressed to loopback as martian: the
    handshake succeeds and nothing else does. The engine test asks for a
    TEST-NET-3 address and lets the server's `freedom` redirect it.
- **A server reached over UDP has no ping.** The list's ping is a TCP
  handshake, which Hysteria 2, WireGuard and mKCP servers never answer, so
  they show «UDP» rather than «нет» (`ServerConfig::over_udp`, `Ping::Udp`).
- **xray answers a SOCKS CONNECT as soon as it accepts it, not when the server is
  reached.** Timing `Outbound::connect_tcp` measured 0 ms (`measure_latency` in
  `engine.rs`): a successful dial proves nothing about the tunnel, and an
  unreachable server shows up later as EOF on a stream that looked connected. The
  doc comment on `DIAL_TIMEOUT` in `outbound.rs` claims it bounds the engine's
  connection to the server, which contradicts that. Settle it with three
  alternating runs against a real server before building timeouts, retries or
  health checks on either assumption. Latency is two `HEAD` requests on one
  kept-alive connection, timing the second; the first pays for the handshakes.

## 11. Where things stand

**Done in stage 1:** renderer spike; profile and link parsing; xray management
and download; relay and connection table; system proxy (three modes, crash-safe
restore); routing (proxy / direct / block by domain, address, port, process,
geosite/geoip) and its screen; server list with ping; copy to clipboard; the
application icon, the status card's picture and the rail's cat; starting with no profile, no
server and no engine, and adding a server from the window; the profile written
by bliss itself, with the system-proxy choice remembered; the rule editor (add,
change, reorder, delete, the fallback) with rules applied under live traffic,
and rules from a right-click on a connection; a switch on every rule, and
presets of their positions, applied from the routing screen, the main screen
or the tray; search and sorting in the
connections table and a resizable log; Ctrl+V for links, lists and
subscriptions; subscriptions (1.6: base64 or plain lists, `subscription-userinfo`
and `profile-title`, a refresh every 12 hours, through the tunnel when it is
up); zoom with the window size; no console window; deleting servers, a
subscription's servers grouped under its name, a rule filter for the
connections table, and log levels in colour; a settings screen, which so far
starts bliss with Windows, minimized or not; scrollbars that draw only a thumb;
the traffic chart as a gliding curve with the rate under the pointer; the tray,
which the window hides into and whose menu bliss draws itself; one window at a
time, a second copy leaving at once (rule 24); links for every kind of
server xray reaches — VLESS, VMess (v2rayN's JSON and the URL form), Trojan,
Shadowsocks (SIP002, the old base64 shape, the 2022 ciphers, obfs-local and
v2ray-plugin as xray transports), SOCKS, Hysteria 2 and WireGuard — over RAW
(with its HTTP disguise), WebSocket, HTTPUpgrade, XHTTP, gRPC and mKCP, under
TLS (pinned, checked by name, ECH) or REALITY (with ML-DSA verification),
each carried end to end through a real xray 26.3.27 in the engine test (§2).
The tunnel already starts with the window (`Setup.autostart`).

**Started in stage 2:** the TUN adapter — wintun inside the binary, the
adapter and its routes, TCP and UDP carried through the same rules and the
same table, a switch under the ring on the main screen (`[tun] enabled`)
and `bliss run --tun`, both honoured only by a copy with administrator rights
(§9) — which the window can be (rule 25). **Never yet run against the real network**: every claim about it here
comes from the code and the unit tests, and the first elevated run is the
user's to make.

**Open:** UDP through the SOCKS inbound (`UDP ASSOCIATE` is implemented for
the *outbound* side only, so a program pointed at the SOCKS port still has no
UDP); the name out of the connection itself (SNI, `Host:`) so domain rules
reach TUN traffic, and fake-IP after it; a privileged helper or service
that owns the adapter and the routes, so TUN no longer needs all of bliss
elevated; IPv6 in the TUN, either carried or blocked; renaming servers;
turning a link that asks for an unchecked certificate into a pinned one (xray
no longer has `allowInsecure`, §10); the rest of the settings screen (ports, the
engine, installing it with a button); an idle timeout for half-open
connections; the first-run wizard (1.8); the 12-hour memory run.

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
