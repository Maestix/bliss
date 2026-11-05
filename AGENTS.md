# AGENTS.md — bliss

Instructions for coding agents working in this repository. Everything here was
learned the expensive way; most entries name the bug that taught it. Every
claim was fact-checked against the code, git and this machine when the file was
written — if you find one that has gone stale, fix the file in the same commit
as the code. Read the whole file before the first edit.

bliss is a proxy client for Windows and Linux, written in Rust. It began as a
VLESS + REALITY client and now reads a link for every kind of server xray-core
can reach: VLESS, VMess, Trojan, Shadowsocks, SOCKS, Hysteria 2 and WireGuard,
over every transport xray still has. Stage 1
(current) drives an external `xray-core` process and owns everything around it:
its own SOCKS5/HTTP inbound, a relay with per-connection accounting, routing,
the machine's proxy settings, and a Dioxus desktop window. Beside the tunnel it
can hold a work VPN — the official OpenVPN, started through OpenVPN's own
service (on Linux through polkit) and kept to bliss's rules (§10a). Stage 3
replaces xray with a native TLS 1.3 + REALITY + VLESS stack behind the same
`Outbound` trait. Windows is where bliss is used every day; Linux is the port
of 9 October 2026, built and tested in WSL, its window first run in WSLg on
11 October (§12).
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
    `socks://`, `hysteria2://`, `wireguard://` — and a subscription's address;
  - an OpenVPN profile (`.ovpn`), which carries a private key and a
    `tls-crypt` key inline: the user's work profile, a friend's (one was
    attached to a message on 4 October and so read whole — the user was told
    to have it replaced), the `[vpn]` section of the real profile, and
    `%LOCALAPPDATA%\bliss\data\openvpn\profile.ovpn`, which bliss writes as
    the VPN starts and deletes once OpenVPN has read it;
  - on Linux the same two files live at `~/.config/bliss/bliss.toml` and
    `~/.local/share/bliss/xray.json` (in WSL too, which is the user's).

  All but the first live outside the repository; never copy them in. The user
  once pasted the head of the work profile, with the `remote` line blanked and
  everything from the certificates on left out: that much is not secret, and
  it is all a question about the profile needs. OpenVPN's own log beside it,
  `openvpn\openvpn.log`, holds no key but names the work server; quote the
  line bliss quotes, not the file. To learn the
  xray config schema, read `config.rs`, not a generated file.
- **Test data is fake by construction**: reserved names (`example.net`,
  `a.example`), TEST-NET-3 addresses (`203.0.113.0/24`), private ranges, literals
  such as `1.2.3.4`, the UUID `11111111-2222-…`, and repeated-letter keys and
  passwords (`AAAA…`, `PPPP…`). Keep new fixtures in that style so nobody has
  to wonder whether a random-looking key is real. The two exceptions need
  working keys and make them at run time: `crates/xray/tests/engine.rs` has
  xray derive them from fixed repeated-byte private keys, and
  `crates/openvpn/tests/service.rs` has OpenSSL make a CA and a certificate
  for each side, and OpenVPN a `tls-crypt` key, in a folder deleted after.
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
  started after that deleted the file and changed nothing. In WSL the
  snapshot is in `~/.local/share/bliss`, the setting in WSL's own dconf
  (`gsettings list-recursively org.gnome.system.proxy`): WSL has a real dconf
  and session bus, so a bliss there sets a system proxy for WSL's programs.
- **Talk to the user in Russian.** Code, comments, docs and commit messages are
  in English.

## 1. Layout

| crate | package | what lives there |
|---|---|---|
| `crates/proto` | `bliss-proto` | pure data: the parsed server — protocol, security, transport — and its redacting types (`server.rs`); share links, one parser per scheme in `link/` (`vless.rs`, `vmess.rs`, `trojan.rs`, `shadowsocks.rs`, `socks.rs`, `hysteria2.rs`, `wireguard.rs`) and the transport and TLS parameters they share (`link/stream.rs`); TOML profile (`profile.rs`), routing rules as written (`routing.rs`), VLESS Encryption parameter (`encryption.rs`), credential redaction (`redact.rs`) |
| `crates/core` | `bliss-core` | engine-agnostic: inbound — SOCKS5 and HTTP on one port (`inbound.rs`, `socks5.rs`, `http.rs`), `Outbound` trait, relay, connection tracker, routing matcher (`routing.rs`), geo `.dat` decoder (`geo.rs`), system proxy (`sysproxy.rs`), port→process lookup (`process.rs`); leaving by one interface (`interface.rs`), dialling through one — the VPN's — with its own DNS servers (`through.rs`, `dns.rs`) |
| `crates/openvpn` | `bliss-openvpn` | the work VPN: the profile rewritten to serve bliss's rules (`profile.rs`), OpenVPN's interactive service and, on Linux, `pkexec` (`service.rs`), its management interface (`management.rs`), one connection from start to finish (`session.rs`) |
| `crates/xray` | `bliss-xray` | xray config generation for every protocol and transport, written for the engine's version (`config.rs`), process supervision tied to bliss's life — a Job Object, on Linux the parent-death signal (`job.rs`) — GitHub downloader; `tests/engine.rs` carries every kind of link end to end through a real xray (§2) |
| `crates/tun` | `bliss-tun` | stage 2: `wintun.dll` carried inside the binary (`lib.rs`), the adapter (`device.rs`; Linux's own TUN device, `linux_device.rs`), the machine's addresses and routes (`net.rs`; over netlink, `linux_net.rs`), packets turned into connections and flows (`stack.rs`), and the lookups answered with addresses read back as names, where bliss is the DNS server (`names.rs`, Android's only so far); `tests/netns.rs` carries a connection and a datagram through Linux's device in a namespace of its own (§2) |
| `crates/app` | `bliss` (binary) | CLI (`main.rs`), engine thread (`engine.rs`), Dioxus window (`ui.rs`), Linux's tray (`tray_linux.rs`) and single window (`instance.rs`), the profile and every change the window makes to it (`profile_file.rs`), rule conditions as typed (`rule_text.rs`), subscriptions (`subscription.rs`), starting with the session (`startup.rs`), log ring buffer (`logs.rs`), clipboard, the stand-ins that keep libxdo out of Linux's binary (`no_xdo.rs`, guarded by `tests/libraries.rs`) |
| `crates/android` | `bliss-android` | the tunnel's engine on Android, `libbliss.so`: its own loop (`engine.rs`), the JSON-lines socket the screens use (`protocol.rs`, `tests/socket.rs`), the three JNI calls and the one back, `ownerOf` (`android.rs`), the rule sets fetched from GitHub (`geo.rs`), and the desktop's `subscription.rs` and `rule_text.rs` by `#[path]`; the app around it is the Gradle project in `android/` (docs/android-stack.md) |
| `crates/spike` | `bliss-spike` | renderer measurement (WebView2 vs Blitz), not in `default-members`; build with `cargo build -p bliss-spike --profile spike`; results in `docs/spike-1.0-renderer.md` |

**The seam:** `bliss-core` and `bliss-proto` never depend on `bliss-xray`; only
the `bliss` binary (`main.rs`, `engine.rs`) calls into it. Existing xray
knowledge outside `bliss-xray`, not to be extended: the `[engine] xray` path in
`profile.rs`, and the decoder for Xray's `.dat` files in `geo.rs`. OpenVPN has
the same seam: only the binary calls `bliss-openvpn`, and what reaches the core
is an address and DNS servers, dialled through by `Through`, which knows
nothing of OpenVPN. One kind is
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
  `geosite.dat`, `xray.json`, `bliss.log`, `system-proxy-backup.txt`,
  `selected` (the server in use, by key), and
  `openvpn\`: `profile.ovpn` while OpenVPN starts, and OpenVPN's own
  `openvpn.log`, written over at every start
- default ports: inbound SOCKS 10808, HTTP 10809; xray bridge 11080
  (`crates/proto/src/profile.rs`)

Runtime paths on Linux, the same calls (`directories`) giving XDG's folders:

- profile: `~/.config/bliss/bliss.toml`
- work dir: `~/.local/share/bliss` — `xray` (no `.exe`, unpacked with its
  exec bit), the rule sets, `xray.json`, `bliss.log`,
  `system-proxy-backup.txt`, `selected`; no `openvpn\` files: the profile goes to OpenVPN
  on its standard input
- WebKit's own data: `~/.cache/<exe stem>/webview` (`linux_config` in ui.rs)
  — left to itself WebKit writes into `~/.local/share/<program>`, which for
  bliss is the work dir
- in `$XDG_RUNTIME_DIR`: `bliss-window-<exe stem>.lock` and `.sock` (the
  single window, `instance.rs`), `<exe stem>-tray/` (the tray icon's picture),
  `bliss-openvpn-<pid>.sock` (OpenVPN's call, while it is made)
- autostart: `~/.config/autostart/bliss.desktop`, and the application menu's
  entry `~/.local/share/applications/<exe stem>.desktop` with its icon in
  `~/.local/share/icons/hicolor/96x96/apps/` — each written on a click in
  the settings, never on launch (`startup.rs`)

## 2. Commands

```bash
cargo fmt --all
cargo clippy --all-targets   # no warnings (default members; --workspace adds the spike)
cargo test                   # default members, ~240 tests
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
- **The work VPN is carried end to end by a real OpenVPN** in
  `crates/openvpn/tests/service.rs`, ignored by default:

  ```bash
  cargo test -p bliss-openvpn --test service -- --ignored --nocapture
  ```

  It needs OpenVPN Community with its interactive service (installed here on
  4 October, 2.7.7), an account in Administrators or "OpenVPN
  Administrators", and OpenSSL — OpenVPN's own if installed with it, else
  Git's, else the PATH; `BLISS_OPENVPN` and `BLISS_OPENSSL` name others, and
  `BLISS_TEST_LOG=bliss_openvpn=trace` shows bliss's lines. A server on the
  TAP adapter and a client on Data Channel Offload, both started through the
  service, the client by `Session` from a profile shaped like the work one;
  a TCP echo and a DNS server behind the server. It checks: a
  profile OpenVPN refuses is said at once in OpenVPN's words; the VPN comes
  up and its profile leaves the disk; nothing the server pushes
  (`redirect-gateway`, a route, a DNS server) reaches the machine while
  bliss's route does; a datagram, 256 KB of TCP and a name only the VPN's DNS
  knows go through `Through` and back, by the client adapter's own byte
  counts; `stop` takes the address and route away; and a session whose
  runtime is shut down under it — bliss dying — takes OpenVPN with it. About
  30 s. Its first run met the bug a friend's machine showed (§10a). It puts
  windows on nobody's screen, but two OpenVPNs and their adapters on the
  machine for that half minute; the routes it checks are all of metric 9999
  or on the VPN's own network.
- `crates/proto/tests/links.rs` snapshots the whole `Debug` output of a parsed
  `ServerConfig`. Changing its fields or the redacting `Debug` impls fails that
  test. `cargo-insta` is **not installed**: insta writes a `.snap.new` next to the
  snapshot — read the diff, confirm nothing credential-shaped became readable,
  then replace the `.snap` (or rerun that one test with `INSTA_UPDATE=always`).
  Never accept a snapshot you have not read.
- A running `bliss.exe` locks the binary: close it before `cargo build`.
- **Releases are built by GitHub Actions** (`.github/workflows/release.yml`),
  asked for by the user so a friend can download bliss rather than build it.
  A tag `v*` builds Linux on ubuntu-22.04 (through `build-linux.sh`; 22.04
  for glibc 2.35, §12) as a `.tar.gz` and a `.deb` (`packaging/linux/make-deb.sh`:
  apt brings the runtime libraries, its `postinst` runs `setcap`), Windows
  on windows-latest as one `.exe`, and Android on ubuntu-latest through
  `android/build-native.sh` with the runner's newest NDK and Gradle 8.14.3,
  and puts them with a `SHA256SUMS.txt` on the tag's release; run by hand
  it only leaves artifacts. Every APK, debug or release, from any machine,
  is signed with `android/bliss.keystore` (password `android`, alias
  `androiddebugkey`): the debug key of the container that built every APK
  before, committed on the user's word — the repository is private and bliss
  is shared among a few friends — so that each release installs over the
  last and over what their phones already have, keeping the app's data.
  A key of each run's own would have meant uninstalling for every update.
  Checked here: the `.deb` built from this container's build installed
  through apt with its capability and came off clean, and the APK signed
  with the committed key. First run on 5 November, v0.5.0: every job green
  at the first try (Linux 5 min, Android 8, Windows 14; 9 min in all with
  the caches warm). **The tag is the user's to push**: from the cloud
  container a tag push is refused (403) and so is a force push, so a
  release is `git tag vX.Y.Z <commit> && git push origin vX.Y.Z` on their
  machine, after the version is raised in Cargo.toml (`[workspace.package]`)
  and android/app/build.gradle.kts (`versionCode`, `versionName`). Commit
  messages carry no `Claude-Session:` line: the user asked for the session
  link to be taken out of every message and the tag's, which took a
  history rewrite on their side.
- **Working from Linux, check Windows' build too.** A test in `stack.rs`
  that used `Gateway::for_tests` — Linux's alone — without
  `#[cfg(target_os = "linux")]` passed every run in a Linux container and
  broke the user's `cargo test` on Windows on 5 November. With
  `rustup target add x86_64-pc-windows-gnu` and mingw's gcc (`apt-get
  install gcc-mingw-w64-x86-64`, for ring's C), `cargo clippy --target
  x86_64-pc-windows-gnu --all-targets` type-checks every crate, the window
  included; it links and runs nothing.

**On Linux, in WSL.** The user's WSL Ubuntu 24.04 is the Linux this port was
built and tested on. Its default Rust (1.86) is too old for the lockfile, so
Rust 1.97.0 sits beside it as a second toolchain (installed 9 October; the
default was left alone). Build from the Windows checkout into a target dir on
Linux's own file system, and switch off the Windows user's cargo config, which
cargo finds by walking up from `/mnt/c/...` (§3, trap 11):

```bash
cd /mnt/c/Users/Maestix/Desktop/Projects/Rust/bliss
export CARGO_TARGET_DIR=$HOME/bliss-target RUSTC_WRAPPER= CARGO_BUILD_RUSTC_WRAPPER=
cargo +1.97.0 test --workspace --exclude bliss-spike   # ~260 tests
cargo +1.97.0 test -p bliss-tun --test netns -- --ignored --nocapture
cargo +1.97.0 build --release -p bliss   # $HOME/bliss-target/release/bliss, 16 MB
```

- **`./build-linux.sh` builds on any common distribution** — asked for by the
  user, who builds on Arch, after a build that stopped halfway on a library
  it lacked. It checks a C compiler, pkg-config, WebKitGTK 4.1, GTK 3,
  libsoup 3, OpenSSL (dioxus-desktop's `native-tls`; missing from the list
  below, which the user's WSL happened to have), libxdo (by linking `-lxdo`,
  since Ubuntu's ships no `.pc`) and Rust 1.88+ (the locked dependencies'
  highest `rust-version`, also `rust-version` in Cargo.toml); offers to
  install what is missing through apt-get, dnf, pacman or zypper, with the
  package names of each (Arch's checked against archlinux.org's package
  search on 5 November, Rust from `pacman -S rust` there rather than
  rustup); then builds `--release --locked`, with WSL's target dir and
  kache switched off when run from `/mnt/…`, and names the `setcap` line.
  `--check` only reports, `--yes` installs without asking, `--debug`.
  Run whole in this container (Ubuntu 24.04 without the window's
  libraries): it installed them through apt and built, and the window's
  103 tests then ran here for the first time. The dnf and zypper names are
  Tauri's documented lists and were not run. Never edit the script while it
  runs: bash reads it as it goes, and an edit made during the build had it
  resume mid-line after cargo finished (a syntax error at line 241).
- **The window needs GTK's and WebKitGTK's development packages**, which take
  `sudo` and so the user's password: `build-essential pkg-config
  libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libxdo-dev
  libayatana-appindicator3-dev librsvg2-dev`, 234 packages with what they
  pulled in. The user installed them on 11 October, from a terminal tab this
  agent opened with the command, typing the password themselves (WebKitGTK
  2.52.6, GTK 3.24.41). The window crate builds and its 94 tests pass there;
  `text_survives_the_round_trip` is Windows' alone, so nothing in that run
  touches the clipboard — which WSLg shares with Windows.
- Before they were in, two stopgaps did: stand-in `.pc` files on
  `PKG_CONFIG_PATH` (name, `Version: 99.0.0`, empty `Libs` and `Cflags`) let
  `cargo check` and `clippy` type-check the window without linking it, and a
  crate of its own outside the repository ran `startup.rs` and `instance.rs`
  by `#[path]`. Neither is needed now; never point a real build at the
  stand-ins.
- **The TUN end to end test needs no root**: it runs itself again under
  `unshare --user --map-root-user --net`, root in a namespace with nothing in
  it, with a dummy interface for an uplink and an echo for an engine. The
  device, its routes — the two halves, one around it, a stale one a "killed
  bliss" left — a connection and a datagram there and back, and nothing left
  after. Before that, a tunnel over another VPN's halves is refused, naming
  its interface, with theirs left as they were; and an address kept around
  the tunnel that already has a route of someone else's keeps it, after the
  tunnel too. Half a second. Ubuntu on a desktop may refuse unprivileged user
  namespaces (`kernel.apparmor_restrict_unprivileged_userns`); WSL does not.
  Seen failing with the stale-route clean-up, with the route removal, and
  with routes added by replacing (each of the last two guards on its own)
  taken out.
- **The window runs in WSLg** (§12 for what was seen). OpenVPN's Linux path
  does not: WSL has no OpenVPN, and polkit there has no agent to ask the
  password with.

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
   write CRLF, and an edit that matched before stops matching after. Python's
   `Path.write_text` is one of them on Windows — text mode turns every `\n` into
   `\r\n` — so a replacement script should write bytes, or be followed by the
   script below. To normalise
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
9. **Everything an agent starts writes AppData into a store of its own.** The
   Claude desktop app is an MSIX package (`Claude_pzs8sxrjxfjjc`), and every
   process it starts — shells, cargo, Python, a probe bliss — inherits its
   file system virtualization: a file or folder created under
   `%LOCALAPPDATA%` or `%APPDATA%`, `Temp` excepted, lands in
   `%LOCALAPPDATA%\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Local\…` (or
   `Roaming\…`), and reads see that copy over the real one. Nothing outside
   the package sees it. Found on 4 October when OpenVPN's service, asked to
   start OpenVPN in a probe's `%LOCALAPPDATA%\bliss\data\openvpn`, answered
   `CreateProcessAsUser`: "the directory name is invalid" (267), while the
   same request from the end-to-end test, in `Temp`, worked: every new folder
   under AppData failed and every old one and every one in `Temp` or on the
   Desktop worked. It also means an agent's `bliss.log` and `xray.json` are the
   store's copies from its own earlier probes, not what the user's bliss
   wrote. `Temp`, the Desktop and the repository are written through.
   - What another process must see — OpenVPN's service, the user's bliss —
     has to be written from outside the package, or into `Temp`.
   - To run a program outside it, have WMI start it:
     `Invoke-CimMethod -ClassName Win32_Process -MethodName Create` with a
     `Win32_ProcessStartup` whose `ShowWindow` is 0, and a `cmd /c set …&&`
     for environment variables. The window probe of the VPN started its
     `bliss-probe.exe` so, and read the real `bliss.log` by having such a
     `cmd` copy it into `Temp`.
   - A sandbox switched off for one command (`dangerouslyDisableSandbox`) is
     still inside the package: the first probe tried that and failed the
     same way.
10. **A child a test leaves running keeps cargo waiting for ever.** A process
    started with inherited handles holds the test binary's own output pipe,
    and cargo waits for that pipe to close. Breaking `kill_on_drop` on purpose,
    to see `an_engine_that_does_not_answer_is_given_up_on` fail, left a
    `waitfor.exe` running and the whole `cargo test` hung for ten minutes,
    until the `waitfor` was ended by hand. When a guard is about a child being
    ended, run its check against the broken code in the background, and end
    the child yourself.
11. **WSL finds the Windows user's cargo config.** Cargo looks for
    `.cargo/config.toml` in every folder above the one it runs in, and from
    `/mnt/c/Users/Maestix/Desktop/...` that includes
    `/mnt/c/Users/Maestix/.cargo/config.toml`, whose `rustc-wrapper = "kache"`
    names a Windows program: every build failed with "could not execute
    process `kache …` (never executed)". `RUSTC_WRAPPER=` and
    `CARGO_BUILD_RUSTC_WRAPPER=` empty switch it off (§2).
12. **`wsl.exe` from Git Bash gets its paths rewritten too** (trap 6): a script
    passed as `/mnt/c/...` arrived as `C:/Program Files/Git/mnt/c/...`. Set
    `MSYS_NO_PATHCONV=1` for every `wsl.exe` call. Its own messages come out in
    UTF-16, with NULs between the letters (`tr -d '\000'`), and every call
    starts with a warning that the Windows `localhost` proxy is not mirrored
    into WSL — which also means WSL reaches the network directly, not through
    the user's v2rayN.
13. **Scripted edits written through the shell broke twice on 9 October**,
    trap 1 again: a `\n` in a test became a real newline in a byte string, and
    a `\\0` became a real NUL in a Rust source file. Write a replacement script
    with the Write tool and run the file; never put one in a heredoc.
14. **Right after `spawn`, a child on Linux can still be its parent.** The
    kernel wakes the parent of a `vfork` (which is how `Command` spawns there)
    a moment before the child's executable is its own, so `/proc/<pid>/exe`
    can name the test binary, and a child that has not yet exec'd holds every
    one of the parent's sockets. Three tests failed, once each, in some
    twenty runs of every test at once: two read `sleep` as the test binary
    (whose name has `bliss` in it), one traced its own socket to a parallel
    test's child.
    A test about a child's identity waits until the child is what it
    became; the lookup gives a shared socket to its older holder.
15. **`wsl.exe -- <command>` runs it through a shell first**, so a `$x` or a
    `$(…)` meant for the inner `bash -c` is expanded, empty, before it gets
    there. Put the commands in a script and run the file (`wsl.exe -d Ubuntu
    -- bash /mnt/c/…/script.sh`), as with trap 13.

## 4. Commits

- Signing is on (`gpg.program = C:\Program Files (x86)\GnuPG\bin\gpg.exe`).
  **Never bypass it.** When signing fails because no agent is reachable, starting
  the Gpg4win agent has fixed it:
  `"/c/Program Files (x86)/GnuPG/bin/gpg-connect-agent.exe" //bye`.
  Git Bash also ships `/usr/bin/gpg-agent`, with a different home directory and
  socket — make sure you are not debugging that one. Confirm with
  `git log --format='%h %G? %s' -1`: it must show `G`.
  GnuPG 2.4 keeps the keys in a daemon of its own: on 30 September signing
  failed with "can't connect to the keyboxd", `gpg-connect-agent //bye` waited
  eight seconds for an agent that never came, and a `git commit` then hung on
  gpg. The next commit signed after three steps — `gpgconf.exe --launch
  keyboxd`, killing the stuck `gpg.exe`, `gpg-agent.exe --daemon` — and which of
  them mattered was not established.
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
  arrive (5 000, `MAX_PROFILE_SERVERS`), names the VPN's DNS answered (256,
  `dns.rs`), an OpenVPN profile (256 KB, `MAX_PROFILE`, measured before the
  file is read), the engine's archive (128 MB, the announced length checked
  before anything is allocated for it and the bytes as they arrive), a file
  unpacked from it (256 MB, whatever its zip header says) and its checksum
  file (64 KB) in `download.rs` — the archive was read whole into a buffer
  sized by the server's word, and each file by the header's. The project
  exists because another client's GUI reached 7.8 GB this way. Linux added
  three: OpenVPN's output kept for why it stopped (64 KB, lines of 4 KB, the
  rest dropped as it arrives, `Tail` in session.rs), the program names
  remembered between socket tables (256, process.rs) and the recent owners
  tried first (8). Android added a command line from a screen (4 MB,
  `MAX_LINE` in its engine, read through `take` so a frame between two
  reads keeps what was read) and the commands a screen holds while the
  tunnel's process starts (32, the oldest dropped, `EngineClient`), and
  the networks with a rule of their own (32, `MAX_NETWORKS`), and the
  servers with a star (256, `MAX_FAVORITES`, the oldest dropped).
- **bliss.log has a cap while bliss runs, not only when it starts**: one run
  writes at most 16 MB into it (`logs::RUN_LIMIT`, a line saying so, then
  nothing; `the_file_stops_at_its_limit`), and at start a file past 2 MB is
  cut to its last 1 MB of whole lines (`logs::keep_tail`) rather than
  deleted, so the end of the run that made it grow survives. It was trimmed
  at start alone: on 18 October a friend's bliss wrote 305 GB of one warning
  into it at some 600 MB/s, until their disk had 12 GB left (§9, an ended
  device). The window's log pane keeps its 400 lines whatever the file does.
  The default filter is `info,ipstack=error`: ipstack warns about every
  packet it cannot hand on, which nobody can act on.
- **The rule-set files are walked, not read whole** (`walk` in geo.rs):
  each entry's head for its name, the entries asked for whole, the rest
  stepped over. runetfreedom's geosite is 85 MB, and the phone compiles a
  table on every rule switch, in its engine loop
  (`the_walk_reads_what_the_whole_file_holds`, ignored, needs
  `BLISS_GEO_DIR`).
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
- **Code after `ui::launch` never runs.** tao ends the event loop with
  `process::exit`, on Windows as on Linux, so the system proxy was "put back on
  the way out" by a line that had never once executed: «Выход» left the
  machine pointed at a dead port until bliss next started and `recover` found
  the snapshot. Found by reading dioxus-desktop for the Linux port on 9
  October. What must happen as bliss leaves happens in `LoopDestroyed`
  (`with_custom_event_handler` in `launch`).
- **On Linux "parent" means the thread.** `PR_SET_PDEATHSIG` signals a child
  when the *thread* that forked it ends, and tokio's threads come and go
  (`block_in_place` hands a worker's core away; idle blocking threads exit
  after ten seconds). The engine is forked from a thread that lasts as long as
  bliss (`job.rs`, `the_engine_outlives_the_thread_that_asked_for_it`), and so
  is `pkexec`, which arms the same signal itself (`service::linux::launch`).

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
  password written beside it. An OpenVPN profile travels as `Secret` in
  `Command::AddVpn` and sits in `bliss_proto::Vpn`, whose `Debug` gives its
  length alone (`debug_does_not_show_a_vpn_profile`,
  `a_vpn_profile_on_its_way_is_not_printed`).
- **On a phone, loopback is everyone's.** Any app reaches 127.0.0.1 with
  nothing but the internet permission, so xray's SOCKS port there serves
  only a login made from `/dev/urandom` for each run of the tunnel's
  process, on a port the system picks (`fresh_login`, `free_port`,
  `bliss_xray::require_login`, `SocksOutbound::with_login`); the rule sets'
  fetch through the tunnel carries it too. Open, any app could go through
  the server around bliss's rules, or find a proxy by knocking — one of the
  ways apps look for a VPN (`the_engine_serves_only_who_has_the_login`,
  ignored, needs `BLISS_XRAY`). The desktop's port stays open: what runs
  beside it runs as the same user.
- **A connection a rule sends to the VPN goes there or nowhere.** With no VPN
  up, `Serving::dial_tunnelled` refuses it (`NotConnected`, SOCKS reply 3,
  HTTP 502 saying the VPN is not connected) rather than letting it fall
  through to the tunnel or out directly; a work site reached some other way
  is exactly what the user did not ask for
  (`a_vpn_route_goes_to_the_vpn_or_nowhere`, seen failing against a fall-through).
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
  naming the rule. A silent drop is an endless spinner. Through the TUN, a
  connection is closed, and a flow of datagrams gets an ICMP "port
  unreachable" from its far end (`port_unreachable`, written to the device
  by `Ended::answer`, as sing-box answers a rejected UDP flow): dropped in
  silence, Chromium waited on QUIC it had seen work and showed the user
  ERR_QUIC_PROTOCOL_ERROR on Google instead of going to TCP
  (`udp_to_443_meets_the_rule`, seen failing without the answer).
- **Absolute-form HTTP requests are forwarded with `Connection: close`**, with
  `Proxy-*`, `Connection` and `Keep-Alive` stripped: the relay has dialled one
  host, and a kept-alive client could send its next request, for another host, to
  the first one.
- SOCKS5 address types are 1/3/4; VLESS uses 1/2/3 for the same three things.
- **A connection's program is found by both its ends** (`name_for_connection`:
  the program's address and the inbound's, or the TUN's two). Linux hands a
  local port already in use to a new connection whenever the pair of ends
  differs, so there the port alone named whichever socket the table listed
  last — another program's, and a `process:` rule decided by it. Windows
  gives each connection a port of its own and asks by the port. Datagrams,
  mostly unconnected, are still asked by their port
  (`a_connection_is_found_by_both_its_ends`, seen failing by the port).
- **A program is the same program with or without `.exe`.** Rules compare
  program names without case and without a trailing `.exe` (`program_name` in
  routing.rs), so a profile carried between Windows and Linux keeps its
  program rules: `Telegram.exe` there is `Telegram` here. Typed in the editor,
  a bare name gets `.exe` on Windows only, and a Linux path (`/usr/bin/…`) is
  read as a program.

**The profile and the work dir**

- **The server in use outlives a restart, by key** (`remembered_selection`,
  `remember_selection`): `selected` in the work dir holds
  `bliss_proto::link_key` of its link — never the link, which lives in the
  profile alone — written whenever the choice moves (a click, a list that
  moved under it), and looked for among the servers that parse at start.
  bliss started on the first server every time; the user found the choice
  lost between runs, and on the phone too, where `selected` was written on a
  tap alone and «Автовыбор», a failover or a network's rule moved the server
  without it (`remember` in its engine). `bliss run` without `--server` takes
  it up as well (`the_server_in_use_outlives_a_restart`, both platforms).
- **A star is `favorite = true` on a `[[server]]`**, written only when set;
  the list shows starred servers under «Избранное» first and not again in
  their section (`server_sections`), the tray's menus put them first
  (`favorites_first`), and a subscription's update keeps the star on the
  servers it still lists (`apply_subscription`). The phone keeps its stars
  by key in android.json instead, since its subscriptions are refreshed by
  its own engine.
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
  - **A subscription can be renamed, given a new address, and copied** — the
    user found it could only be refreshed or deleted. The editor
    (`SubscriptionEditor`) starts with the address box empty and the host as
    its placeholder: the window still never shows the token, a new address
    replaces the old (`Command::EditSubscription`, the address in a `Link`),
    and the old one is had whole only from «Скопировать адрес», which puts it
    on the clipboard from the engine (`CopySubscription`), since the frame
    never holds it.
  - **A name given in the window stays.** The provider's `profile-title`
    replaced the name at every update, so a rename would have lasted twelve
    hours; `renamed = true`, written only when it is, keeps it, and an empty
    name hands the naming back (`edit_subscription`,
    `a_subscription_is_renamed_and_moved_in_place`, seen failing without the
    flag).
  - **A list from an old address is dropped.** `FetchDone` carries the address
    it fetched; one that lands after an edit moved the subscription is not
    applied to it, and the fetch from the new address keeps the card's
    "loading".
- **Assume another bliss shares the work dir** — a test run, a stray copy, the
  user's own instance. `bliss.log` is appended, and trimmed (to its tail) only if an open with
  `share_mode(0)` succeeds, because Rust opens files with `FILE_SHARE_DELETE` and
  deleting a log another process writes to succeeds silently. Linux has no
  sharing modes: every bliss holds a shared `flock` on the log it appends to,
  and a trim goes ahead only if an exclusive one can be had. The system-proxy
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
  On Linux it succeeds — the running engine keeps the file it opened — so
  `DownloadError::engine_in_use` is Windows' alone (error 32 is EPIPE there).
- **The system proxy on Linux is the desktop's** (`sysproxy.rs`, `imp` for
  Linux): GNOME's `org.gnome.system.proxy` through `gsettings`, written
  wherever dconf keeps it — Firefox reads it on every desktop, Chromium on
  the GNOME family — and on Plasma KDE's `kioslaverc` too, through
  `kwriteconfig5`/`6` and the `reparseSlaveConfiguration` signal. A desktop
  with neither says so (`NoDesktopProxy`). The snapshot is the same file in
  the same `key=value` lines, each key naming its store (`gnome:…`, `kde:…`),
  so it goes back into the stores it came from; Windows' keys are the
  `enable`/`server`/`bypass` every earlier build wrote, so an old snapshot
  still restores. Two keys are written only to keep the old setting from
  misleading programs around bliss's: GNOME's `use-authentication` (off, so
  the old proxy's login is not sent to bliss; the login itself is never read)
  and KDE's `ReversedException`. Only stand-in `gsettings` and
  `kreadconfig`/`kwriteconfig` scripts have run it (`gnome_settings_go_back_as_they_were`,
  `kde_settings_go_back_as_they_were`); no desktop has.
  - **Each store goes back on its own** (`imp::still_ours`): on Plasma
    GNOME's settings (Firefox's) and KDE's are two, and a person who turned
    the proxy off in Plasma's settings left Firefox on bliss's dead port when
    one signature decided for both (`each_store_goes_back_on_its_own`).
  - **Settings that cannot be read are not settings someone changed**: the
    snapshot stays for the next start, where it used to be forgotten
    (`a_snapshot_that_cannot_be_read_back_is_kept`).
  - **A running copy is a running bliss**, not any process with its number:
    pids here are handed out in order from boot, so after a restart the
    snapshot's owner was whatever started at that point, which kept the
    snapshot for ever and had `set` refuse over a copy that was not there.
    `process_alive` reads `/proc/<pid>/exe` for a name with `bliss` in it
    (`a_reused_number_is_not_a_running_copy`).
  - Its tools run with `LC_ALL=C`: their messages are read for what they say.
- **The system proxy is changed off the engine loop, one change at a time.**
  On Linux a change is a chain of programs — some thirty on Plasma, each
  allowed seconds — and every click waited behind them. `SystemProxy` in
  engine.rs takes wishes through a watch, so only the latest waiting one is
  applied, in a blocking task; the choice is on screen at once, whether it
  took once it has. Inside the process `sysproxy` takes one change at a
  time (`CHANGING`): the window puts the settings back on its own thread as
  bliss ends, and a restore that read them halfway through a `set` would
  forget the snapshot and leave the machine on bliss's port. After the
  restore on the way out (`leave`) nothing is set again
  (`nothing_is_set_once_bliss_has_left`, whose work dir cannot hold a
  snapshot, so that a broken guard stops before the machine's settings). The
  panic hook restores only if no change is under way (`restore_unless_busy`):
  waiting could hang the very thread that panicked.
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
  see the window go. So do raising the adapter (§9) and changing the system
  proxy (above).

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
- **On Linux the entry is an XDG autostart file**, `bliss.desktop` in
  `~/.config/autostart`, written whole on a click and renamed into place. The
  desktop's own switch is in the file — `Hidden=true` (the spec's) or
  `X-GNOME-Autostart-enabled=false` (GNOME's) — and reads as off, as Task
  Manager's does. A path the `Exec` line would need two layers of escaping for
  (`"`, `` ` ``, `$`, `\`, `%`) is refused in words rather than escaped wrong.
  `BLISS_STARTUP_KEY` names a scratch folder there.

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
    proxy" are both pink, which is why "direct" must never borrow `ON`. The
    work VPN is the one hue that was left, a calm sky blue (`--color-vpn`,
    `VPN_TONE`, `#7eb3ee`); `the_vpn_is_offered_when_there_is_one` checks
    app.css has every action's colour for the dot, the segment and the pill.

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

    **Flags are Noto Color Emoji's, as on the phone.** Windows' emoji font
    has none, so 🇳🇱 in a server's name came out as the letters «NL»; the
    user asked for the phone's look. `assets/noto-flags.woff2` (710 KB, COLRv1)
    is the part of Noto Color Emoji that Google Fonts serves for U+1F1E6–1F1FF,
    259 flags, resubset with fontTools to drop `vhea`/`vmtx`; Google's own
    GitHub was not reachable from the container. theme.css declares it as a
    second face of "Comfortaa", for that range alone and with Comfortaa's own
    weights (`300 700`): Chromium joins a family's faces by unicode-range only
    within one set of weights, and the first build's `100 900` kept the flags
    from ever loading — the user still saw «NL», and headless Chromium showed
    the face `unloaded` (Playwright, a page with theme.css and both fonts
    inlined). So nothing else in the window names it; `THEME_STYLES` inlines it as it does
    Comfortaa, and `OFL-NotoColorEmoji.txt` stays beside it. A geoip entry in
    a rule reads as the phone's does, flag and Russian name (`countries.rs`,
    CLDR's names through Babel, its pseudo regions left out).

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
    `.search`), and clickable things get `cursor: pointer` from `app.css`,
    and nothing sets one inline (`no_cursor_is_set_inline`) — not even the
    window's own chrome, since the title bar went (rule 31).
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
    - **And seen only once it is painted.** The render is Dioxus's and the
      paint WebView2's, later, and a hidden window's paints are not presented:
      shown straight from that effect, the menu stood for its first 10 to 70ms
      as the program cut to its shape, in every one of ten openings, in this
      build and the one before (screen grabs under the window, §7). Waiting
      two animation frames fixed later openings but not the first after the
      program had been on screen. Now the window is shown cloaked
      (`DWMWA_CLOAK`: up, painted into, left out of the picture by DWM) and
      uncloaked when `PAINTED` answers — four animation frames, 200ms at most
      — and the program comes back out of the tray the same way
      (`BRING_BACK`). Measured after: no foreign frame in three first
      openings, five more and five returns of the program; the menu now
      appears about 100–150ms after the click
      (`a_window_is_seen_only_once_it_is_painted`).
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
    - **The menu shows no line meant for the program** (`toast_shown`). The
      line at the foot of the window is drawn over everything, and the menu
      is that window: TUN switched on without administrator rights, the
      window closed to the tray, and the menu opened over the refusal
      (`the_trays_menu_shows_no_line_meant_for_the_program`).
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

26. **A page nobody can see is not drawn.** The window spends most of its
    life in the tray, and a sample of the user's bliss there, a day into a
    run, spent 2.2% of a core, half of it in WebView2 — every frame rebuilt
    the page, and WebView2 laid out and painted a window that was not on the
    screen. Now:
    - `App`'s frame loop draws a frame only while `seen` — the program on the
      screen and not minimized, or the tray's menu — and otherwise marks the
      page `BEHIND`. The tray icon's tooltip still follows, through
      `TOOLTIP`, which re-runs `Tray` alone.
    - Whatever shows the window from the tray calls `catch_up` first: the
      latest frame, in the same handler, so the menu is drawn from it in the
      render that puts it on the page. The program's window is shown at once
      and redrawn a moment later; a restore from the taskbar catches up on
      its `Focused` or `Resized` event, and the loop itself on the next frame.
      `frames_are_drawn_only_where_they_are_seen` holds both, and failed
      with one `catch_up` taken out.
    - Out of sight, WebView2 is told it may give memory back
      (`MemoryUsageLevel::Low`, `rest`), and told otherwise on the way back:
      Microsoft's switch for an inactive app, which keeps its scripts
      running — the menu and the keys need them.
    - The connections table goes into the frame only while its screen is
      open and the page can be seen (`Command::Table`, `TABLE_OPEN`,
      `Frame.table`): up to 512 closed connections and every live one were
      copied twice a second into the frame, and again into every component
      it is handed to, on every screen and in the tray. The count of live
      ones then comes from the tracker
      (`the_table_is_copied_only_when_asked_for`).

    It was built without measuring, as the user asked, and checked on
    1 October in a probe window: no change to the page in 4s in the tray or
    3s minimized, against changes every frame on screen; the latest numbers
    within 0.35s of a click on the icon and 0.3s of a restore from the
    taskbar; WebView2's private memory 104 MB on screen, 76 MB after 10s in
    the tray. The flash of the program in the menu's first frames, suspected
    of coming from the memory level, was in the build before it too, and is
    rule 23's to answer.

27. **On Linux the window is WebKitGTK's, and the tray is not the window.**
    Read in the sources of tao, wry, tray-icon, muda and dioxus-desktop on
    9 October, and first run on 11 October in WSLg, which has no tray (§12):
    what is said below about the tray is still read, not seen.
    - **The window is built hidden and shown by bliss** (`SHOW`, in an effect
      of `App`). dioxus-desktop builds every window hidden on Windows and
      shows it after the first render, but on Linux builds it as asked, and
      a page in a window built visible applies each change on an animation
      frame (`headless` false in dioxus's interpreter) and waits for the
      page to say so before it polls the program again. A hidden window gets
      no animation frames: the first change after the window went into the
      tray was the last, every `use_future` stood still — the tray's menu,
      a second copy's raise — and only a click on «Открыть» woke it. Built
      hidden, the page applies changes as they come, as on Windows. Shown
      through tao, which shows what GTK packed into the window too.
    - **The tray is a native menu** (`tray_linux.rs`). tray-icon drives an
      AppIndicator, which reports no clicks — the host opens the item's menu
      itself — shows no icon at all without a menu, and panics inside
      `build()` when its library is missing. So rule 23 is Windows' alone:
      Linux gets an icon with a menu of the main switches (state, connect,
      open, servers — 30 at most —, presets, system proxy, quit), kept in step
      by `tray_linux::MODEL`, which the frame loop updates in the tray too,
      where the page gets no frames. Clicks arrive through
      `use_muda_event_handler`; dioxus routes the tray's menu there too, and
      `use_tray_menu_event_handler` never fires. A check item ticks itself
      when clicked, so every click puts the ticks back as the engine has them.
    - **No tray host, no hiding.** `tray_linux::host` asks for the library
      (`dlopen`) and for someone on the session bus taking
      StatusNotifierItems — GNOME as Fedora ships it has nobody; Ubuntu's
      extension and Plasma are. Without both, the close button leaves bliss
      (`WindowCloses`, the proxy put back in `LoopDestroyed`) and a start in
      the tray opens minimized instead — after the first render (`ICONIFY`),
      so rule 20's guard holds on every platform. A start in the tray, which
      is the one at sign-in, waits up to ten seconds for the host while the
      library is there (`SIGN_IN`): Plasma starts a session's programs
      beside the service that takes their icons, and the one answer decides
      the whole run.
    - **A second copy brings the first up** (`instance.rs`): it sends `show`
      and its launcher's activation token over the claim's socket, and the
      first `present`s its window with the token — through GTK, since tao's
      `set_visible` then `set_focus` drops the focus, and Wayland gives a
      window that asks without a token "bliss is ready" instead. On a desktop
      with no tray it is the only way back to a hidden window. `present`
      shows what is inside first (`show_all`): GTK's `present` shows the
      window alone, and one that started in the tray, never shown, came up
      empty — tao packs the page into a box that is shown only with it.
    - **A signal leaves as «Выход» does.** SIGTERM ends every program of a
      session as it closes, and it ended bliss where it stood: nothing put the
      proxy back, and the next session started on bliss's dead port. glib
      hears SIGTERM, SIGHUP and SIGINT on the window's thread
      (`leave_on_signals`) and closes the window as the last one, so
      `LoopDestroyed` runs. A display that dies first still ends bliss
      without it; the startup check is for that.
    - **WebKitGTK knows `-webkit-user-select` only**; app.css says both.
    - **Its GPU path draws a blank window** under WSLg, NVIDIA and some
      Wayland compositors. `main` sets `WEBKIT_DISABLE_DMABUF_RENDERER=1`
      first thing, before any thread, unless the person set it; dioxus's own
      switch sets it later, races the threads and sends children to X11, and
      `launch` turns it off.
    - **WebKit's data would land in the work dir** (`~/.local/share/bliss` is
      its default for a program called bliss): `launch` gives it
      `~/.cache/<exe stem>/webview`, and the window a ground colour, since on
      Linux it is shown before the page is.
    - **The clipboard is GTK's**, on the window's thread: a copy from the
      engine's thread goes through `glib::MainContext::invoke`, and a paste is
      a request answered later — `paste` is `async` everywhere — because
      `wait_for_text` would run a main loop inside tao's.
    - Expect from tao on Linux: no `ScaleFactorChanged`, so `WindowIcons`
      does nothing there; `is_minimized` stays false under Wayland, so a
      minimized window still counts as `seen` and draws; no window positions,
      keep-above or skip-taskbar under Wayland; an icon from the window only
      under X11 — Wayland takes it from a `.desktop` file, which bliss writes
      when asked (§12). Whether WebKitGTK throttles a hidden page's timers
      and animations, and whether tao's edge resizing works over the page,
      are open.

28. **One spacing scale** (the note above `SCREEN_HEAD` in ui.rs): 12px
    between any two cards, the rail and the screen included (a screen is
    `padding:18px 20px 18px 12px`), 16px inside a card, 12px inside a tile
    within one. Every screen's heading row is `SCREEN_HEAD`, 34px at least,
    so the first card starts at one height with or without a button beside
    the heading; a screen's scrolling column is `SCROLL_COLUMN`, reaching into
    the right padding by its scrollbar so its cards end where the others do.
    A checkbox row reaches 10px into its card's padding for its hover ground
    (`CheckRow`), so the card's text starts at the card's edge. The user found
    14, 15, 18 and 20px cards side by side, settings cards 9px narrower than
    the rest, and the rail 20px from everything.
29. **The server list's columns are dragged** (`ServerColumns`): grips in
    the header between the name and the address, and between the address
    and the protocol; a double click puts one back. The header measures the
    list (`onresize`) and the columns are pixels from then on, the address
    taking what is left, so a row always fits
    (`dragged_server_columns_still_fit_the_list`). Text is cut in code to the
    measured width at 0.6em a letter (`letters_in`), the whole of it in the
    tooltip. Seen in the window on 28 October: a long VPN name shown whole
    once dragged wide, a drag far past a limit with no dead stretch on the
    way back, the reset. Widths last until bliss closes, as the log's height
    does.
30. **One thing is said in one place.** The program's name and mark head the
    rail (`.rail-brand`, the mark inlined like the pictures), and the
    engine's version is in the rail's card alone — the title bar said both,
    and then went (rule 31). The main screen has no heading: its two cards stand level
    with the top of the rail, and a failure's red note goes under them. The
    connection is one card: the server's name (cut in code to the room the
    page leaves it, `viewport()`, less an estimate for what shares its
    line) with the presets and the VPN's chip beside it, which give way to
    the name and hide what no longer fits; the state; the address; and how
    it hides with the fingerprint at the end (`server_detail`) — the name,
    the presets and the chip had stood in the screen's heading, and the
    fingerprint among the numbers. Measured in the window at 1040 and 880px:
    the cards' tops on the rail's, the name never clipped mid-letter.
    The address says a name once (`address_line`): it read `host ·
    host:443` whenever the TLS name was the server's own host. The numbers'
    card has the current rates where the fingerprint was. The listening
    ports sit in the card's lower corner, absolutely, so the name's line
    has the width. **The server list is a screen of its own** (`Servers`,
    `ServerList`), with nothing over it: it was a tab under the status cards,
    which took a third of the window from a list of dozens, and the rail's
    «Серверы» was a shortcut to that tab. The connection screen's tabs are
    «Трафик» and «Действия».
31. **There is no title bar.** The rail and the cards reach the window's top
    with the 18px they keep at its foot. The window's buttons are small
    rounded plates floating in the top-right corner (`WindowControls`,
    `.wctl`: 98×24, 24px down and 26px in), level with the screens'
    headings; `SCREEN_HEAD` keeps its contents 116px clear of them, and on
    the main screen they sit inside the numbers' card, whose rows start 38px
    down. The window is dragged by `DragStrip`, the 18px along its top edge,
    and by the head of the rail; a double click on either maximizes. What a
    heading cannot fit beside them in a narrow window is `.head-hint`,
    hidden under 980px. Measured on every screen at 1040 and 880px: 12px
    from the nearest thing in a heading to the buttons, none over a row.
32. **The address check is one line over the rule list** (`Checker`): the
    connections screen's search box, and the answer beside it — the action,
    the rule, the target — with a country's note on a second line. It was a
    card 288px wide and the screen's height; the rules have that width now.

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

**`BLISS_WORK_DIR` moves the work dir** — the engine, `xray.json`,
`bliss.log`, the system-proxy snapshot, `openvpn\` — for the window and the
CLI alike. Pointed at an absolute folder in `Temp`, a probe shares nothing
with the user's bliss, needs no WMI (§3, trap 9: `Temp` is written through),
and can start with no engine at all. The probe of 7 October did: the
missing engine offered for install, the install refused in words (its proxy
variables pointed at a dead port, so nothing was downloaded), the engine
copied in from outside and taken up 2.8 s later, then the subscription
editor. It did not click «Скопировать адрес»: the user's clipboard held more
than text, and a probe leaves such a clipboard alone.

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
`tray_icon_app` in bliss's own process — a top-level window of that class in
the tray-icon bliss builds with now, which `EnumWindows` finds; an earlier one
made it message-only, so walk `FindWindowExW(HWND_MESSAGE, …)` as well — and
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

More probes that worked:

- **A flash of the wrong content** is caught by grabbing the screen under the
  window from the moment `IsWindowVisible` says it is up, as fast as
  `PIL.ImageGrab` goes (every 40–50ms), and comparing each grab with one taken
  once it has settled and with the screen grabbed under it before the click:
  a cloaked window shows what is behind it, which is not a flash. The
  rectangle includes the invisible borders, so the user's own screen around
  the window moves the numbers; with a game running behind, look at the odd
  frames before believing them. Each run has to start from the state it is
  about: the menu's first opening after the program was on screen differs
  from every opening after it.
- **Window icons:** `WM_GETICON` on the `bliss` window, then `GetIconInfo` and
  `GetDIBits`; compare the pixels with the frame of `assets/bliss.ico` at that
  size. A zero difference proves the resource is in use and the Dioxus logo is
  not.
- **The work VPN in the window** needs the probe started outside Claude's
  package (§3, trap 9), or OpenVPN's service cannot see the folder the probe
  writes the profile to. The probe of 4 October: a test server and its keys
  as in the end-to-end test, started through the service by a short
  PowerShell over the pipe; an HTTP server and a DNS server bound to the
  server's VPN address; a scratch profile whose `[vpn]` holds the client
  profile as a TOML literal string (`'''`) with `enabled = true` and a rule
  sending `intranet.example` and the VPN's network there; `bliss-probe.exe`
  started by WMI. Then the chip's class (`busy`, `up`, `failed`), `curl`
  through the probe's SOCKS and HTTP ports (`--noproxy ""`), the chip clicked
  off and on, and the probe killed with the VPN up. Stop at the first
  failure: a probe that waits out every timeout after the VPN failed only
  wastes the user's half minute. What it writes outside lands in the real
  work dir — `xray.json`, `bliss.log`, `openvpn\` — and the probe's WebView2
  folder, `%LOCALAPPDATA%\bliss-probe`, which it removes after, as it does
  the test log in `openvpn\`.
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
  is gone by the time a packet exists, so a `domain` or `geosite` rule sees
  only what names itself in the connection; `ip`, `geoip`, `port` and
  `process` see everything. Two ways back to the name. **Fake-IP** —
  answering the DNS query and handing out an address bliss can recognise
  again — exists on Android only (`names.rs`, given to the stack as
  `Carrying::names`), where the VPN service names bliss's own DNS address.
  The desktop's adapter does not answer DNS, on purpose: bliss's own
  lookups and xray's would get the pool's addresses too, and the user's DNS
  is dnscrypt-proxy on loopback, which no adapter sees. **The name out of
  the connection** works on both (`sniff.rs`, below): on the desktop
  whenever the table has a rule of names (`Carrying::sniff`,
  `Router::has_name_rules`, `the_desktop_reads_a_name_out_of_the_connection`),
  which is HTTPS and HTTP on the ports sites are on, with QUIC refused so
  the browser takes TCP. **A name's address comes from the name** (FNV-1a over
  198.18.0.0/16, the next free on a clash): handed out in turn, the
  addresses meant other names after the tunnel's process restarted while a
  browser kept its answers, and Google closed on the user
  (`a_name_keeps_its_address_across_a_restart`). A pool address that is
  still forgotten is asked for its name like a bare address and dialled by
  it, never as itself (`a_forgotten_pool_address_is_dialled_by_the_name_it_says`). There a name also meets `geoip:` and subnet rules: when
  such a rule comes before the decision (`Router::wants_address`), the
  stack asks 1.1.1.1 through the tunnel for the site's address — never the
  phone's network — keeps it ten minutes (`names::Addresses`, 4 096 at
  most; a name nothing could be learnt about, a minute), and decides by both
  (`decide_with`). Only a rule that could meet a site's public IPv4
  address counts (`sees_sites`): a country, or a block outside the private
  and special ranges. 0.2.3 counted «Локальная сеть» and the IPv6 refusal
  too, so nearly every site was looked up and, on a server that lost the
  UDP, waited five seconds for it — the user found parts of GitHub not
  loading (`the_local_network_and_ipv6_are_no_reason_to_look_a_site_up`).
  The lookup goes over UDP, then TCP, 1.5 s each, and one at a time per
  name: the connections after a lookup wait for its answer
  (`Carrying::asking`, `a_name_is_asked_once_however_many_wait`) rather
  than each asking through the server again. A table built with
  `build_resolving` gives no warning that a country rule cannot see sites.
  A connection that still arrives as a bare address there — a «Частный
  DNS», a browser's own DNS over HTTPS, an address the app kept — is asked
  for its name instead (`sniff.rs`): the first bytes on ports 443, 80, 8443
  and 8080, up to 16 KB and 400 ms, for a ClientHello's `server_name` or a
  request's `Host:`, then dialled straight to the address it chose or by the
  name through the tunnel, the bytes read passed on first. QUIC to a bare
  address is refused, so the program falls back to that TCP.
- **A rule's `network` is a limit, not a condition** (`tcp`, `udp`, as
  xray and v2rayN write it): every other condition is "any one is enough",
  while `network` bounds them all, so `network = ["udp"]` with `port =
  ["443"]` is QUIC and never HTTPS on the same port, and `network` alone is
  every flow of it (`udp_and_443_is_quic_not_https`). The TUN's datagrams
  are decided as UDP (`Router::decide_flow`); `decide` is TCP, which is
  all the SOCKS and HTTP ports carry. The phone's profiles carry a
  built-in «QUIC — открывать по TCP», UDP and 443 → block, under «Локальная
  сеть» (`quic_rule`; offered once to older profiles,
  `the_quic_rule_is_offered_once`): QUIC is UDP inside the tunnel, which
  Vision drops on 443 and other servers carry badly, and parts of GitHub
  did not load (`udp_to_443_meets_the_rule`). The user chose this shape over
  0.2.6's `protocol = ["quic"]`, which is still read — as UDP and 443,
  `Rule::settle_protocol` — and never written. And there,
  unlike the desktop, a rule change ends the open connections the new table
  sends elsewhere (`until_rerouted`, `Serving::router_changes`): an app
  keeps its connections for minutes, and the user's `geosite:reddit` rule
  read as one that did not work (`a_bare_address_is_decided_by_the_name_it_says`,
  seen failing with the reroute taken out). While the adapter is on, the routing screen names the rules that see
  only HTTPS and HTTP through it (`blind_under_tun`): the ones whose only
  conditions are domains and geosite sets. A rule that also has an address,
  a port or a program still decides with those, so it is left out rather
  than buried in the list.
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
- **An ended device is the end** (`Ended` in stack.rs). A wintun session that
  has been shut down answers every read at once, with nothing — `Ok(0)`, for
  ever (`ReadState::Closed` in wintun-bindings' async session). ipstack's
  loop takes each answer for a packet, cannot hand it on once bliss has
  stopped taking streams, logs `process_device_read error: Send Error channel
  closed`, and reads again without once yielding — so the abort `Tun::drop`
  sends it never lands, and it runs until the process ends. Switching TUN off
  shuts the session down while the task that takes the streams is still being
  cancelled, a race: a friend's bliss hit it on 18 October and wrote 305 GB
  of that line at some 600 MB/s, and they could not make it happen again.
  `Ended` turns the first empty read or failure into a wait that never ends,
  said once in the log, and the stack's task yields and is aborted. With a
  device that ended bare, the stack read it 158 828 times in 0.2 s; through
  `Ended`, once (`an_ended_device_is_read_once`). The cap on bliss.log (§5)
  is the second wall.
- **Still missing, and each is a visible hole:** IPv6 is neither carried nor
  blocked, so a machine with working IPv6 reaches IPv6 sites around the
  tunnel; DNS still goes out the physical interface, which is a leak Windows
  makes worse by asking every adapter at once; and there is no service or
  helper, so TUN needs the whole of bliss running as administrator.
- **On Linux the device is the kernel's own** (`linux_device.rs`):
  `/dev/net/tun`, one `TUNSETIFF` (`IFF_TUN | IFF_NO_PI`), its address, mask,
  MTU and flags by ioctl, and the descriptor read and written through tokio's
  `AsyncFd` — one read a packet — straight into ipstack, with no compat layer.
  No new dependency: the `tun` crate (WTFPL) would have been one. The rights
  are `CAP_NET_ADMIN`, read from `CapEff` in `/proc/self/status`, and the
  window's words name the fix: `sudo setcap cap_net_admin+ep <path to
  bliss>`, again after every update, since a new file has no capability.
  Running the window as root is no answer: pkexec takes `WAYLAND_DISPLAY`
  away, and root's dconf and session bus are not the user's.
- **Its routes go over netlink, written by hand** (`linux_net.rs`), never
  through `ip`: a program started by a `setcap`'d bliss does not inherit the
  capability. Four messages: a route added (`NLM_F_CREATE | NLM_F_EXCL`,
  as `ip route add`), one taken away, the kernel's answer to
  "which way to this address" (`RTM_GETROUTE`, unprivileged, policy rules
  included — `gateway` asks it for `1.1.1.1`, and each address kept around
  the tunnel is asked for on its own, before the tunnel's routes exist), and
  the whole table. Every route carries `rtm_protocol` 98 (`proto 98` in `ip
  route`): the device's routes die with it, but the ones around it do not, so
  the next tunnel takes away any route of that number first
  (`net::forget_stale`).
- **A route is added, never replaced.** One of the same prefix and metric
  that is there after `forget_stale` is another program's. Replaced — the
  first version did, to take over what a dead bliss left — it carried
  number 98 and went with bliss's routes when the tunnel stopped: another
  VPN's halves of the address space, and its traffic out around it until it
  reconnected. Now a tunnel over another's halves is refused, in words that
  name its interface ("another VPN?"), and an address kept around the
  tunnel that already has a route of its own keeps it, untouched after.
- **A direct dial under the tunnel is bound to the uplink** with
  `SO_BINDTOIFINDEX` (`SO_BINDTODEVICE` by name before Linux 5.0), which an
  ordinary user may set since 5.7: bound, a socket is routed among its
  interface's routes alone, and the tunnel's halves never see it — no fwmark,
  no policy rule, nothing to undo.
- **What the Linux tunnel does not know about**: OpenVPN's own connection
  (§10a) — its process is root's, and no table names it for a user's bliss —
  and the DNS systemd-resolved sends per link. Neither has been looked at on
  a real machine.

## 10. xray-core facts

- Nearly every release is flagged prerelease, so "latest" can be months old. The
  downloader installs the newest **stable** release; `--newest` takes any.
- Checksums in `.dgst` are labelled `SHA2-256=`.
- **The window installs the engine itself, and takes up one installed from
  outside.** A missing engine used to be one red line naming
  `bliss engine install`; run from PowerShell, that windowed program gives the
  prompt back at once and prints after it, and once it had finished the
  window went on waiting for a click to look again — the user ran it and saw
  nothing happen. Now the line has a button, «Установить движок»
  (`EngineInstall`, `Command::InstallEngine`): the newest stable release —
  the newest of any kind when none of the last thirty is stable — fetched off
  the loop with its progress in `Frame.engine_setup`, a failure said in
  Russian by kind (`install_problem`), the tunnel started after. And the
  ticker takes up an engine that appears while the window waits for one,
  two seconds after it shows, for the archive's other files to land.
  Either way the rules are compiled again (`took_up_engine`): geoip.dat and
  geosite.dat came in the same archive, and rules about countries had been
  compiled from none.
- **Downloads are timed by silence**, not length: 20 s to connect and 60 s
  without a byte. The 300 s on the whole request cut off a 15 MB archive on
  any link under 50 KB/s.
- **`xray version` has 20 s to answer** (`check_version`), and is ended if it
  does not (`kill_on_drop`). The first run of a downloaded engine can wait on
  an antivirus, and `start` awaits this check in the engine loop: unbounded,
  it held every click in the window
  (`an_engine_that_does_not_answer_is_given_up_on`, seen failing without the
  kill).
- Xray now calls `streamSettings.network` `raw` where it used to be `tcp`
  (`config.rs` emits `raw`, `link/stream.rs` accepts both); the `mixed` inbound
  serves SOCKS and HTTP on one port.
- `kill_on_drop` does not survive `TerminateProcess` of the parent; the engine
  runs in a Job Object with kill-on-close (`job.rs`).
- "Ready" means three things (`process.rs`): the bridge port was free before
  spawn, the child is still alive, and whatever answers speaks SOCKS5 and
  takes the method the config asked for (no-auth, or a login).
- **One engine holds the bridge port at a time.** On the phone every start,
  switch and stop is a turn on `Port` in its engine, in the loop's order:
  a stop that only began the old xray's shutdown let the next start find
  the port taken, and the tunnel failed (`turns_on_the_port_run_in_order`).
  The desktop awaits `parts.stop()` before `start` for the same reason (§5).
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

## 10a. The work VPN (OpenVPN)

The user was given a `.ovpn` by their workplace, for its tools in the browser,
and asked for it in bliss. xray has no OpenVPN, and OpenVPN is a different kind
of thing — it carries packets, and by default takes the whole machine — so it
is driven, not spoken: the official client, started through its own service,
told what it may not do, and dialled through by `bliss_core::Through` for the
connections a rule sends there (`action = "vpn"`). Everything below was read in
OpenVPN's and OpenVPN GUI's source (`src/openvpnserv/interactive.c`,
`validate.c`, `src/openvpn/options.c`, `route.c`, `push.c`, `manage.c`,
`doc/management-notes.txt`; OpenVPN GUI's `openvpn.c`) on 30 September, at 2.8
development. On 4 October OpenVPN Community 2.7.7 was installed here, and all
of it has since met a real OpenVPN — a server of the test's own on this
machine, through the real service, in the end-to-end test (§2) and in the
probe window (§7) — but not the work server.

The first real run was a friend's, and it failed as «OpenVPN не вышел на
связь за 20 с»; the end-to-end test then failed the same way, and found
why (the second point below). What the real runs taught:

- **The service's access list on `openvpn.exe` grants the user
  QUERY_INFORMATION but not QUERY_LIMITED_INFORMATION**, alongside
  TERMINATE, SYNCHRONIZE, VM_READ and READ_CONTROL. Windows checks the very
  bits asked for, so opening it with LIMITED — the right everyone asks for
  first — is refused (5); even PowerShell's `Stop-Process` cannot end it.
  `process::image_name` falls back to the wider right
  (`a_process_with_its_own_access_list_is_named`), `service::terminate` asks
  for TERMINATE with it, and OpenVPN's call is now known by its connection's
  own row in the machine's table: the process the service started, by id
  (`process_of`, `answer`).
- **That refusal was the friend's failure.** OpenVPN called within 25ms,
  bliss read no name for the caller and turned it away as a stranger, the
  leash broke and OpenVPN left, and bliss waited out 20 seconds for a call
  that could not come again. The `terminate` meant to end it failed the same
  way, so the test's first server outlived the test.
- **What OpenVPN says before it calls is said only in its own log.** bliss
  passes `--log` before `--config`: OpenVPN writes there from the moment it
  reads `--log`, and a line of the profile it refuses is refused while the
  profile is read — with `--log` after it, that line went nowhere. On a
  failure bliss reads the end of that log (64 KB at most) for the line that
  says why — an options error, a failed call, the line before "Exiting due
  to fatal error" — and names a line of the profile by its number in the
  person's own file (`PREAMBLE`). The service's pipe is watched while bliss
  waits for the call, so an OpenVPN that exits first is reported in
  milliseconds, not after 20 seconds.
- **The profile leaves the disk once OpenVPN has called**: it was read
  whole by then, keys included, and a reconnect does not read it again (only
  a SIGHUP would, and nothing sends one). It used to stay until the VPN went
  down, or for good if bliss was killed.
- **The address takes no socket for about a second after CONNECTED**: binding
  to it is refused until Windows has made sure no one else has it. `Up` waits
  for a socket to bind (5 s at most); a rule's connection in that second
  used to fail.
- **The hold OpenVPN starts in is released once.** It announces `>HOLD` as it
  calls; answered with a second `hold release` while it was still printing
  its history, OpenVPN 2.7 ran `log on all` again inside it, and every
  warning reached bliss's log twice
  (`the_hold_it_starts_in_is_released_once`).
- **A TAP adapter keeps an address it was given by DHCP**, after the
  OpenVPN that asked for it is gone; the test's server uses `ip-win32 netsh`,
  which the service undoes. With a server killed earlier still holding the
  first TAP adapter, the service made a second one, "OpenVPN TAP-Windows6
  #2", which is still on the machine; removing it takes an elevated
  `tapctl delete`.

- **The interactive service starts it without rights.** `\\.\pipe\openvpn\service`
  takes one message — working directory, options, standard input, each ending
  in a NUL, UTF-16, at most 4096 units — and answers `0x00000000`, the process
  id in hex and `Process ID`, or an error code and two lines of why. It
  appends `--msg-channel`, through which OpenVPN asks it for the adapter,
  addresses and routes, and it undoes all of that when OpenVPN exits. bliss
  checks the pipe's server is the service's own process before writing, as
  OpenVPN GUI does (`GetNamedPipeServerProcessId`).
- **Any options and any profile location only for an authorized account**: a
  member of Administrators or "OpenVPN Administrators" — and a UAC-filtered
  admin token counts, since the group SIDs are compared without looking at
  the deny-only flag, with the local group's members as a fallback. Anyone
  else gets a short list of options and profiles in OpenVPN's own folder
  only, which bliss cannot write; its refusal is shown saying which group.
- **The leash is `--management-client`.** bliss listens on a loopback port and
  OpenVPN connects to it; when that connection closes OpenVPN throws itself
  SIGTERM (`management-exit`). So a bliss that dies takes its VPN with it
  instead of leaving it up with nobody to switch it off — measured: a probe
  window killed with the VPN up, and the VPN's address was gone 0.3 s later.
  A caller that is not the process the service started is turned away.
  `--management-hold` makes it wait until bliss is listening, and `hold off`
  — the flag survives restarts — keeps a reconnect from waiting for bliss
  again. OpenVPN warns at every start that a TCP management port has no
  password; bliss leaves that line out of its log, since the port is on
  loopback and the caller is checked by its process.
- **The profile is rewritten, not trusted** (`profile.rs`). `pull-filter` is
  first-match-wins by prefix, so bliss's filters go first: the server's
  `redirect-gateway`, routes, `dhcp-option`, 2.6's `dns`, `block-outside-dns`
  and `register-dns` are dropped, and `route-method`, as OpenVPN GUI does, since
  a pushed one would take routes away from the service. The same directives
  written in the profile are commented out, with script hooks and anything
  that fights bliss for control (`management*`, `log`, `service`). Everything
  else — ciphers, keys, keepalives — is left as written.
- **One route of bliss's own**: `route 0.0.0.0 0.0.0.0 vpn_gateway 9999`.
  `IP_UNICAST_IF` makes Windows look for a route among those through the bound
  interface only, so a socket bound to the VPN's adapter needs one covering
  the destination; this one's metric is far above any ordinary default
  route's, and TUN adapters' halves of the address space win on prefix before
  metrics are compared, so nothing else picks it. OpenVPN accepts any metric
  of at least 0.
- **`route-method exe` is harmless under the service**: `--msg-channel` comes
  after the profile on the command line and sets the service method; the
  user's profile has that line and keeps it.
- **DNS comes from the log.** The server's DNS servers are filtered off the
  machine, but OpenVPN logs the whole `PUSH_REPLY` at verb 3 (`D_PUSH`) before
  filtering, and bliss reads them there: `dhcp-option DNS` or 2.6's
  `dns server N address`. Names for the VPN's connections are asked of those
  servers through the VPN (`dns.rs`); without any, the machine's own resolver.
  The CONNECTED state gives the adapter's address, and the adapter is found by
  it (`interface::index_of`).
- **One OpenVPN at a time.** A connection replaced or switched off leaves
  without the engine loop waiting (`Session::leave`), and the next waits for
  it (`after`): two at once would fight over the adapter.
- **With TUN on, `openvpn.exe`'s own traffic goes around it** (`stack.rs`,
  `VPN_PROGRAMS`), or the VPN would reach the work network through the proxy
  server; `the_vpns_own_connection_goes_around_the_tunnel`. Unmeasured, and
  probably not the whole answer: with Data Channel Offload — which a UDP
  profile gets on Windows 11, the client log says "ovpn-dco device opened" —
  the connection to the server belongs to the driver, not to `openvpn.exe`,
  so no table names that program for it. A route around the tunnel to the
  server's address, as the engine's server gets, would not depend on that.
- **Not yet:** a login, a one-time code or a key passphrase — such a profile is
  refused in words (`ProfileError`), and a prompt at run time ends the
  connection; IPv6 through the VPN; the VPN's switch in the tray's menu.

**On Linux** there is no service. Read in OpenVPN's, polkit's and the
kernel's sources on 9 October; never run (§12).

- **OpenVPN runs as root through `pkexec`**, which asks for an administrator's
  password in the desktop's own dialog at every start (polkit's default for
  `org.freedesktop.policykit.exec`), or directly when bliss is root. pkexec
  replaces itself with OpenVPN, so the pid bliss started is OpenVPN's. It
  answers 126 for a dismissed dialog and 127 for no right or no polkit agent,
  and both are said in words (`service::linux::exit_words`). It arms the
  parent-death signal itself, so it is started from a thread that lasts
  (`launch`, §5). An OpenVPN running as root cannot be ended from here:
  `signal SIGTERM` over the management interface, and the leash, are all.
- **The profile goes in on OpenVPN's standard input** (`--config stdin`), and
  the key never touches the disk; OpenVPN names a refused line `stdin:<n>`,
  which `their_line` maps back as it maps the file's path on Windows.
- **There is no `--log`**: a root OpenVPN's log would be root's. Its output is
  read instead, the last 64 KB of it kept in memory for why it stopped.
- **It calls over a unix socket** in `$XDG_RUNTIME_DIR` (`bliss-openvpn-<pid>.sock`),
  and the call is taken only when the kernel says the caller is the process
  bliss started and is root (`SO_PEERCRED`) — `process_of` cannot answer for
  a root process's sockets, whose descriptors are closed to a user.
- **The profile gains a line everywhere**: `ignore-unknown-option` for the
  options only Windows' OpenVPN knows (`ip-win32` above all), which Linux's
  refuses as unknown. On Windows it changes nothing — the real-OpenVPN test
  passed with it on 9 October.
- **`ROUTE` goes into a `dev tap` profile only**, on Linux: a socket bound to
  a tunnel is sent down it even with no route there (the kernel takes the
  destination to be on the link), while a second default route becomes the
  machine's own the moment the real one goes.
- Not done there: the TUN's way around OpenVPN's own connection (§9), and
  bliss's DNS for the VPN has only ever met Windows.

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
starts bliss with Windows, minimized or not; subscriptions renamed, given a
new address and copied; the engine installed from the window, and taken up
when installed from a console; scrollbars that draw only a thumb;
the traffic chart as a gliding curve with the rate under the pointer; the tray,
which the window hides into and whose menu bliss draws itself; one window at a
time, a second copy leaving at once (rule 24); links for every kind of
server xray reaches — VLESS, VMess (v2rayN's JSON and the URL form), Trojan,
Shadowsocks (SIP002, the old base64 shape, the 2022 ciphers, obfs-local and
v2ray-plugin as xray transports), SOCKS, Hysteria 2 and WireGuard — over RAW
(with its HTTP disguise), WebSocket, HTTPUpgrade, XHTTP, gRPC and mKCP, under
TLS (pinned, checked by name, ECH) or REALITY (with ML-DSA verification),
each carried end to end through a real xray 26.3.27 in the engine test (§2).
The tunnel already starts with the window (`Setup.autostart`). A window in
the tray draws nothing and lets WebView2 give memory back, and the
connections table is copied only for its screen (rule 26; not measured).

**Run against a real OpenVPN, not yet against the work server:** the work
VPN (§10a) — a `.ovpn` added from the settings screen or with Ctrl+V, OpenVPN
started through its service with the server kept off the machine, a fourth
action «через VPN» in the rules, the connection menu and the editor, a chip
in the main screen's heading that switches it, and its connections in the
table as `VPN`. On 4 October, against OpenVPN 2.7.7 and a server of its own on
this machine: the end-to-end test (§2), and a probe window started outside
Claude's package (§3, trap 9) whose chip brought the VPN up in 2.8 s, whose
SOCKS and HTTP ports carried a rule's connections by address and by a name
only the VPN's DNS knew, which refused them with the VPN off (HTTP 502, SOCKS
reply 3), and which took OpenVPN with it when killed. A friend's first run
had failed on the access list (§10a); the build with the fix has not been
back to them yet.

**Started in stage 2:** the TUN adapter — wintun inside the binary, the
adapter and its routes, TCP and UDP carried through the same rules and the
same table, a switch under the ring on the main screen (`[tun] enabled`)
and `bliss run --tun`, both honoured only by a copy with administrator rights
(§9) — which the window can be (rule 25). **Never yet run against the real network**: every claim about it here
comes from the code and the unit tests, and the first elevated run is the
user's to make.

**Open:** UDP through the SOCKS inbound (`UDP ASSOCIATE` is implemented for
the *outbound* side only, so a program pointed at the SOCKS port still has no
UDP); fake-IP on the desktop's adapter, which first needs bliss's own
lookups and xray's kept off it (the name out of the connection, SNI and
`Host:`, is there since 5 November); a privileged helper or service
that owns the adapter and the routes, so TUN no longer needs all of bliss
elevated; IPv6 in the TUN, either carried or blocked; renaming servers;
turning a link that asks for an unchecked certificate into a pinned one (xray
no longer has `allowInsecure`, §10); the rest of the settings screen (ports, the
engine — updating it, since the button only installs a missing one); an idle
timeout for half-open connections; the first-run wizard (1.8); the 12-hour
memory run. The window's install has been run against a dead proxy only: a
real download from GitHub through it has not been made.

**Linux, ported on 9 October:** see §12.

**Android, designed on 31 October, built from 1 November** (the history
is in docs/android-stack.md; what follows is the plan as it stood):
`docs/android-design.md`, beside a canvas of clickable mockups on claude.ai.
Before it can be built it needs names for domain rules under a VPN service
(fake-IP through the service's own DNS), OpenVPN inside bliss's service
(Android runs one VPN at a time, and the libraries are AGPL or GPL), and
`app:` rules by package name. The stack, chosen on 1 November, is
`docs/android-stack.md`: the tunnel in a process of its own around this
core and libXray, the screens in Jetpack Compose, `Frame` and `Command`
over a unix socket — which first means moving them, and the engine loop,
out of the desktop binary into a crate both can use.

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

Its ClientHello was read on 1 November (v0.8.1, at the user's ask; source
only, nothing built or run). It matches Go uTLS byte for byte by its own
oracle, and the JA4 computed from its committed `chrome` hello is
`t13d1516h2_8daaf6152771_d8a2da3f94cd`, xray's own for Chrome 133. But
every profile pins one extension order and one pair of GREASE values
(`0x0a0a`, `0x1a1a`) for every connection — the draw uTLS makes from a
zeroed random source, which its oracle uses — where Chrome and uTLS
(`ShuffleChromeTLSExtensions`, a GREASE seed per connection) draw anew
each time. JA4 cannot see that; JA3 and anyone watching a few
connections can, and every install shares the one draw. Its test
`every_fingerprint_emits_a_stable_extension_order` holds the wrong belief
that no real client reorders. Until that changes, xray stays: the user is
glad bliss runs the original rather than a reimplementation.

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

## 12. Linux, as of 11 October

The user asked for Linux support on 9 October. Everything Windows-only
already sat behind `cfg(windows)` with stubs elsewhere, so the shared crates
compiled on Linux from the start; the work was giving each stub its Linux
body. The behaviour of the libraries underneath — tao, wry, tray-icon, muda,
dioxus-desktop on Linux; the kernel's TUN and netlink; OpenVPN, pkexec and
polkit; GNOME's and KDE's proxy settings — was read in their sources by two
research agents first, and the answers are in §5, §6 rule 27, §9 and §10a.

**Ported, and run on Linux** (WSL Ubuntu 24.04, kernel 6.6, Rust 1.97.0):

- the engine's leash (`job.rs`), seen failing with the signal taken out and
  with the fork moved off the lasting thread; its file name (`xray`) and exec
  bit;
- which program holds a connection, from `/proc` (`process.rs`), by both
  its ends: the parsers are tested on every platform, the lookups against
  this process and `sleep`, the two caps against their limits, and a socket
  a child inherited counted as the parent's (trap 14);
- leaving by one interface (`interface.rs`), checked by the name the kernel
  gives back;
- the system proxy (`sysproxy.rs`) against stand-in `gsettings` and
  `kreadconfig`/`kwriteconfig` scripts: written, read back as bliss's, and put
  back to the last line, a password beside them never copied; each store
  put back on its own, an unreadable snapshot kept, and a running copy told
  from a process that inherited its number;
- starting with the session and the application menu's entry (`startup.rs`),
  in a scratch folder, and the single window and its raise (`instance.rs`),
  on a scratch lock and socket;
- the TUN device and its routes (`crates/tun`), end to end in a namespace;
- the pieces of OpenVPN's Linux path that run without OpenVPN: its options,
  the caller check by `SO_PEERCRED`, the capped tail of its output.

**The window, in WSLg on 11 October**, on scratch paths (below), native
Wayland and again under X11 (`GDK_BACKEND=x11`):

- it opens — built hidden, shown after the first render — and is drawn as on
  Windows: Comfortaa with its Cyrillic, the pictures, the gliding chart with
  its readout under the pointer, the rule editor's dialog inside the window;
- the screens and the rail's highlight; the settings' Linux words; the
  starting rules added to the profile, with the warnings a work dir without
  geo files gives; the live log; maximized, the page grows (rule 16);
- the two checkboxes write the autostart entry and the menu's, with the
  96 px mark, quoted path and `StartupWMClass`;
- a second copy leaves in 65 ms, and the first logs the ask
  ("window: another copy asked for it"). It does not come back from
  minimized: WSLg's compositor lets no client un-minimize itself, under
  Wayland or X11, and a copy started from a script has no activation token.
  On a desktop the launcher gives one; unseen;
- SIGTERM: "signal 15: leaving", out through `LoopDestroyed`, exit 0;
- WSLg has no GPU here: Mesa warns about EGL and ZINK, WebKit draws in
  software, and it is fine.

**Not run anywhere**: the tray and its menu (WSLg has no tray host, so the
close button leaves bliss there), the clipboard, a tunnel through a real
server, OpenVPN through pkexec, the system proxy on a real GNOME or KDE, TUN
from a `setcap`'d window. Say so when any of it comes up, and take the
user's first run as the test it is.

To see the window in WSL: build as in §2 and run it with `BLISS_PROFILE`,
`BLISS_WORK_DIR` and `BLISS_STARTUP_KEY` on scratch paths, and
`XDG_CACHE_HOME` too, or WebKit's data lands in the user's WSL home — a probe
otherwise writes the user's WSL profile, work dir and autostart. Run it from
a terminal tab the user can see and stop (`run_in_terminal`, `wsl -d Ubuntu
-e bash <script>`): the window lives as long as that `wsl` does. The default
proxy mode leaves WSL's dconf alone; check `gsettings get
org.gnome.system.proxy mode` before and after anyway. To look at the window
from Windows, it belongs to `msrdc.exe` — that is what to grant a screen
tool. Typing into it through injected Unicode text arrives as nothing, and
key presses arrive in the user's layout (Russian: `foo` is `ащщ`); type
nothing that matters that way.

**The release build** is made in WSL (§2), about three and a half minutes
from cold with fat LTO, and was first handed to the user on 15 October, on
their Desktop as `bliss`.

- **A build made on Ubuntu 24.04 needs glibc 2.39.** Rust's standard library
  names `pidfd_spawnp` and `pidfd_getpid` weakly, but the binary's version
  need on `GLIBC_2.39` is not marked weak, so the loader refuses an older
  glibc outright: Ubuntu 22.04, Debian 12 and Mint 21 cannot start it. A
  build for those has to be made on one of them.
- **It needs no libxdo** (`no_xdo.rs`). muda links it for Copy and Paste
  menu items bliss never shows, and the link alone put `libxdo.so.3` among
  the libraries the binary could not start without; Arch's xdotool ships
  `libxdo.so.4`, so the first release build could not have started on Arch.
  dioxus-desktop takes muda with its default features, so the link stays;
  three stand-in functions in the binary leave the linker nothing to take
  from the library, and it drops it. `tests/libraries.rs` reads the
  program's own `DT_NEEDED`: a unit test's binary has no `main` of bliss's,
  drops libxdo either way, and passed with the stand-ins gone. The build
  still needs `libxdo-dev`, to link against before dropping it.

For a person running it: the runtime libraries are WebKitGTK 4.1 and GTK 3
(`libwebkit2gtk-4.1-0` on Ubuntu, `webkit2gtk-4.1` on Arch, which brings
GTK, libsoup and the rest), and optionally `libayatana-appindicator3-1`
(Arch: `libayatana-appindicator`) for the tray — on GNOME with the
AppIndicator extension too (`gnome-shell-extension-appindicator`) —,
`openvpn` and polkit for the work VPN, and `setcap cap_net_admin+ep` on the
binary for TUN. Nothing is packaged
yet: no `.deb`, and no AppImage (whose mount ignores file capabilities, so TUN
would need another way there; the autostart entry names the AppImage's own
file, from `APPIMAGE`, rather than its mount). The settings screen puts
bliss in the application menu on a click — «Значок в меню приложений»,
`~/.local/share/applications/<exe stem>.desktop` and the mark beside it in
`icons/hicolor/96x96/apps` — which is also where a Wayland desktop takes the
window's icon from: the file is named after the program, which is the app id
GTK gives the window.
