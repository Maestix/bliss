# bliss on Android — the stack

Chosen on 1 November 2026, for the design in `docs/android-design.md`. The
user asked for three things in this order: little battery, beauty and
animation, and — perhaps — the web, as on desktop, if it can be quick.
Nothing here is built yet; every number below that was not measured says so,
and the first spike (at the end) is there to measure them.

## The answer

**Two processes. The tunnel lives in one of its own, around the Rust core
that already exists, with xray beside it as on desktop; the screens are
Jetpack Compose in the other.** Not the web: on Android it would not save a
milliampere, and it would cost the things the design leans on most.

```
 main process — only while someone looks          :tunnel process — while the tunnel is up
┌─────────────────────────────────────┐          ┌──────────────────────────────────────────────┐
│ Jetpack Compose screens (Kotlin)    │          │ VpnService (Kotlin glue, a few hundred lines)│
│   ↑ StateFlow<Frame>  ↓ Command     │  unix    │   Builder.establish() → TUN fd               │
│ bliss-android (Rust, UniFFI) ───────┼─socket───┼─▶ bliss engine (Rust): the same Command/Frame │
│                                     │ postcard │     bliss-tun Packets(fd) → ipstack          │
└─────────────────────────────────────┘          │     Router::decide → relay → tracker         │
                                                 │       ├─ direct: plain sockets               │
 the tile, the widgets, the notification ───────▶│       ├─ proxy: xray (libXray, Go) on        │
 are served from :tunnel, so a tap on the        │       │  loopback SOCKS, as on desktop       │
 tile never starts the screens                   │       └─ VPN: OpenVPN, later (below)         │
                                                 └──────────────────────────────────────────────┘
```

## Where the battery goes

**Into the tunnel, not the screens.** On Android a screen exists only while
someone looks at it; the VPN service runs for hours with the phone in a
pocket. Whatever the screens are made of, they cost nothing once closed —
their process is cached, then killed — so the choice of UI toolkit decides
how the app feels and how much memory it takes while open, and almost
nothing about the battery. What decides the battery is the tunnel process,
and these rules, each with a test or a measurement to come:

1. **The tunnel process loads no UI toolkit.** No WebView, no Compose: the
   Rust core, libXray and the Kotlin glue around `VpnService`. It is small,
   which is also what keeps Android's low-memory killer away from it.
2. **Nothing ticks while the screen is off.** Rate sampling, the chart's
   history, pings and the notification's numbers stop on `SCREEN_OFF` and
   start again on `SCREEN_ON`. A tokio runtime with no timer armed sleeps
   in the kernel, which is the point. The desktop's `TICK` loop is the thing
   to cut: on a phone it would wake the CPU twice a second for hours.
3. **No network on a timer.** Every wake of the cellular radio keeps it at
   high power for seconds after the last byte, so Автовыбор measures on a
   start, a failure and the screen coming on; subscriptions refresh through
   WorkManager with a network constraint, batched with everyone else's work,
   instead of the desktop's twelve-hour timer inside the engine.
4. **No wake locks.** A VPN service needs none: traffic wakes the CPU.
5. **Fewer packets per megabyte.** The TUN's MTU is the service's to choose
   (`Builder.setMtu`); the larger it is, the fewer packets ipstack handles
   for the same bytes. Measure CPU per megabyte at 1500 and at a large value
   before picking one.
6. **One copy less, later.** The path is TUN → ipstack → a loopback TCP
   connection → xray → the network: two userspace stacks and a loopback hop
   per connection, plus the Go runtime. It works and is what desktop does;
   the in-process outbound — stage 3's native VLESS, or xray-rust, which
   AGENTS.md notes already carries TUN on Android — takes the hop and the
   second runtime away. Measure the cost first; build it only if it shows.
7. **Fake-IP**: the service names its own DNS server inside the tunnel
   (`addDnsServer`), so no connection waits for a DNS round trip, and
   domain rules see names (the design's first promise).
8. **The log stays in memory.** The 400-line ring, as on desktop; the file
   is written when someone shares it, or on a debug switch. A release build
   logs warnings.
9. **Frames only to someone looking.** The engine publishes frames over the
   socket only while a screen is subscribed and resumed, and the connection
   table only while its screen is open — desktop's rule 26, which there
   saved CPU and here saves the battery.
10. **Go's memory is capped** (libXray's run call has taken a memory limit),
    so its
    collector does not grow the process until Android kills it, nor run
    without need.

## Why two processes

- **The screens' memory is not the tunnel's.** Compose with the pictures
  loaded is tens of megabytes; when the person leaves, Android can take it
  all back without touching the tunnel.
- **A crash in a screen does not drop the tunnel**, and a crash in the
  tunnel is a service Android restarts (and, under always-on VPN, restarts
  itself).
- **The tile and the widgets live with the tunnel.** A `TileService` runs in
  the process its manifest entry names; declared in `:tunnel`, a tap on the
  tile connects without starting a single screen. Widgets are plain
  `RemoteViews` built there on a change of state — Glance would pull the
  Compose runtime into the process that is meant to have none.
- **The boundary already exists.** On desktop the window never touches tokio
  (§6 rule 7): it reads `Frame`s from a watch channel and sends `Command`s.
  On Android the channel becomes a unix socket in the app's private
  directory, with the same two types serialised by `postcard`, and Rust on
  both ends — the screens' process loads a small Rust library that speaks
  the socket and hands Kotlin a `StateFlow<Frame>` through UniFFI. No
  protocol is written twice.

What that asks of the code: `Frame`, `Command` and the engine loop live in
`crates/app/src/engine.rs`, inside the desktop binary. They move into a
library crate both binaries use — desktop's window and Android's service —
which is the first refactor of the port and touches no behaviour.

Mullvad's Android client is the nearest precedent: a Rust daemon in the VPN
service, a Compose UI, Kotlin and Rust meeting over JNI, and Android's own
IPC between the parts. Its sources are public, and worth reading before the
socket is written.

## The tunnel process

- **`VpnService` is Kotlin, and thin**: `prepare`, the `Builder` (address,
  routes, DNS, MTU, the disallowed apps), `establish()`, and the fd handed to
  Rust. Everything after the fd is the core.
- **The fd is bliss-tun's Linux device already.** Android's TUN is the
  kernel's, `IFF_NO_PI`, one packet per read; `linux_device.rs` reads it as
  `Packets(AsyncFd<OwnedFd>)`, and Android's fd is an `OwnedFd`. What the
  Linux code does with ioctl and netlink — address, routes — the `Builder`
  does instead, so on Android only `Packets` is used.
- **bliss's own sockets go around the tunnel by one line**: the service
  excludes its own package (`addDisallowedApplication(packageName)`). The
  engine's direct dials and xray's connections to the server then use the
  real network with no `protect()` call per socket, no `IP_UNICAST_IF`, no
  route around the tunnel — desktop's §9 traps, gone. A VPN app's own
  traffic should stay allowed under «Block connections without VPN», or it
  could not reach its server; the spike confirms it on a device.
- **Routes cover IPv6 too** (`addRoute("::", 0)`): carried, or answered with
  a refusal so apps fall back to IPv4. Desktop's TUN leaks IPv6 (§9); on a
  phone, where mobile networks hand out IPv6 first, that would be most of
  the traffic.
- **xray is libXray**, XTLS's binding built with gomobile: one call that
  takes the JSON `bliss-xray` already writes, a loopback SOCKS port as the
  bridge, and the readiness check desktop's `process.rs` already does. Its
  README warns that no other independently built Go library may share the
  process — which rules out putting another Go engine (sing-box, a Go
  OpenVPN) beside it, and is one more reason the tunnel has a process of its
  own.
- **OpenVPN, later.** It has to live inside this service (one VPN at a time),
  and bliss sends it only the connections a rule chooses, which means
  OpenVPN 3 Core's client API with its tunnel handed to a userspace stack
  rather than to an interface. AGPL or GPL, as the design says; the first
  versions ship without the work VPN, and say so where the desktop has it.

## The screens: Jetpack Compose

Measured against the candidates on what the design actually needs:

| | Compose | Tauri 2 + a light JS framework | Dioxus mobile | Flutter |
|---|---|---|---|---|
| battery, app closed | the same for all four: the screens' process is gone, the tunnel is elsewhere | | | |
| memory while open | the app's own process only | plus a WebView renderer process | plus a WebView renderer process | plus its engine |
| cold start | native; Baseline Profiles precompile the first screen | WebView start, then the page | WebView start, then the page | engine start |
| animation at 90–120 Hz | on the RenderThread | smooth for `transform` and `opacity`; anything that relayouts drops frames | as Tauri | smooth (Impeller) |
| sheet drag and fling, predictive back, keyboard insets, haptics | in the toolkit | hand-written in JS, or a plugin per thing | hand-written, plus JNI | mostly in the toolkit |
| tile, widgets, notification | Kotlin, the same language | Kotlin anyway | Kotlin anyway, harder to wire in | Kotlin anyway |
| what desktop lends it | the core, the words, the tokens | the core, the CSS | the core, the CSS, some Rust habits | the core |
| on Android today | the platform's own toolkit | stable since Tauri 2.0 (2024), Kotlin plugins | "still quite experimental", in its own docs | mature |
| languages in the repo | Rust, Kotlin (and xray's Go) | Rust, TypeScript, Kotlin (and Go) | Rust, Kotlin (and Go) | Rust, Dart, Kotlin (and Go) |

- **Battery does not choose** (above). What is left is how it feels, and the
  phone design is made of exactly what a WebView does worst and Compose has
  built in: a sheet that follows the finger and flings on release, the back
  gesture's preview, a sheet riding up with the keyboard, haptics, pull to
  refresh, a list reordered by its grip.
- **Kotlin is in the project whatever the screens are**: the service, the
  tile, the widgets, the notification, the network callback. Compose adds no
  language; the web would add TypeScript.
- **What desktop lends is the core, not the window.** The phone design
  departs from the window on purpose (the design's last section), so very
  little of `ui.rs` would carry over even into a WebView. What does carry —
  the theme's tokens, the Russian words, the meaning of every state — is
  shared by the measures below, not by the toolkit.
- **If it has to be the web**, it is Tauri 2 with SolidJS (no virtual DOM,
  a few kilobytes) and the canvas's own CSS, under the same two processes —
  the tunnel never in the WebView's process. Animate only `transform` and
  `opacity` (the desktop's ring animates `box-shadow`, which repaints every
  frame), virtualise every list, keep the WebView warm behind a native splash,
  and budget for writing the sheet, the back gesture and the keyboard by
  hand. It can be quick; it cannot be free.

### How Compose gets the look

- **Tokens generated from `theme.css`.** A build step reads the `:root`
  block and writes `BlissColors.kt`, so the phone cannot drift from the
  desktop — AGENTS.md's "they mirror the tokens, so change both together",
  made mechanical. Radii and spacing are the phone's own (the design's
  table).
- **Comfortaa as a variable font** in `res/font`, weights 600 and 700, the
  same subset the window ships plus what the phone draws. It still has no
  tabular figures: the clock and the rates get the same fixed-width digit
  boxes desktop uses (`FIGURE_EM`).
- **The pictures as WebP**, cut for the phone: the sky needs a portrait cut
  of the generated original at 3× (the design notes the wide card's file is
  stretched); the cat and the bunny keep their surface-coloured grounds and
  sit only on surface cards.
- **Words in Android's resources**, Russian plurals in `<plurals>` (one, few,
  many — the same three forms as desktop's `plural`). Meanings that both
  windows show — a ping's band, a state, a refusal's kind — come from the
  core in the `Frame`, so neither window decides them on its own.

### Animation that costs nothing between frames

- **Only layer properties animate**: alpha, scale, translation through
  `graphicsLayer`, so a frame of animation is the RenderThread moving a
  picture, with no recomposition and no layout.
- **The ring**: the glow is a radial gradient drawn once, not a blur; a state
  change springs the colour (`animateColorAsState`); connecting pulses the
  scale, only while connecting; connected gives the heart one spring
  overshoot and the haptic tick together.
- **The state word** changes with `AnimatedContent` (fade and a short rise),
  and the server card opens into its sheet as one shared element.
- **The chart**: the path is built once per sample in `drawWithCache` and the
  glide is a translation animated over the tick, which redraws nothing —
  the shape desktop's rule 22 arrived at, on a toolkit made for it. It
  runs only while its screen is resumed.
- **No blur in lists, no shaders on the screen that stays open**, and every
  animation honours the system's "remove animations" setting.
- **Startup**: R8, Baseline Profiles generated by a Macrobenchmark run over
  Главная and the server sheet, and the first frame drawn before the socket
  answers (the last frame the tunnel published, kept on disk, is shown at
  once).

## Building it

- **Rust**: `cargo-ndk`, targets `aarch64-linux-android` (phones) and
  `x86_64-linux-android` (the emulator); 32-bit ARM only if someone asks.
  UniFFI for the screens' small library; the service's library is plain JNI
  with a handful of functions (start with an fd, stop, socket path).
- **xray**: libXray as an AAR from gomobile, pinned by version like the
  desktop's engine; its API is a single JSON `Invoke` whose shape has changed
  between versions, so the Kotlin call sits behind one function.
- **16 KB pages**: Google Play has required them since November 2025 of apps
  targeting Android 15 and newer, for every native library. NDK r28 and later
  link Rust's libraries that way; whether gomobile's output is aligned has to
  be checked, not assumed — `zipalign -c -P 16 -v 4` on the APK and
  `readelf -l` (`LOAD` alignment `0x4000`) on each `.so`, in CI.
- **Gradle**: Kotlin 2, Compose through its BOM, an App Bundle so each phone
  downloads its own architecture's libraries — xray's is the heaviest part
  of the package.
- **minSdk 29** (the design's floor, for `getConnectionOwnerUid`); targetSdk
  whatever Google Play requires at the time.
- **Whether the service needs a foreground-service type** of its own on
  Android 14: the system binds a VPN service itself, and the documentation
  read on 1 November did not settle it. The spike answers it on a device.

## The first spike, in this order

1. **A tunnel with no screens**: the service, the fd into bliss-tun, the
   core, libXray, one server from a link pasted into a debug field, a
   notification with a switch. Then measure, phone unplugged: battery over
   an idle night and over an hour of video (`dumpsys batterystats`, Battery
   Historian), wakeups with the screen off, CPU per megabyte at two MTUs,
   memory of the process. The rules above are claims until these numbers
   exist.
2. **The socket and one screen**: Главная in Compose, frames over the
   socket, cold start and dropped frames measured by Macrobenchmark on a
   cheap phone, not only a fast one.
3. **The rest of the screens**, the tile, the widgets.
4. **The work VPN**, once the licence is decided.

## Sources read on 1 November

- [Google — Always-on VPN: disallowed apps lose network under «Block connections without VPN»](https://support.google.com/work/android/answer/9213914?hl=en)
- [Android — Support 16 KB page sizes](https://developer.android.com/guide/practices/page-sizes)
- [XTLS/libXray](https://github.com/XTLS/libXray)
- [Tauri — Mobile plugin development](https://tauri.app/develop/plugins/develop-mobile/)
- [dioxus-mobile 0.7.0-alpha.2 — "Android support is still quite experimental"](https://docs.rs/crate/dioxus-mobile/0.7.0-alpha.2)
- [Mullvad — Android developer posting: Compose, Kotlin and a Rust daemon over the NDK, Messenger IPC](https://webbjobb.io/jobb/237550/android-developer-at-mullvad-vpn-gothenburg)
- [Android 14 — foreground service types](https://developer.android.com/about/versions/14/changes/fgs-types-required)

## The first build, 1 November

What exists, and where it left the plan above:

- **`crates/android`** (`libbliss.so`): its own engine loop rather than the
  desktop's, which lives inside the desktop binary with the window, the tray,
  the system proxy and OpenVPN. It reuses bliss-proto, bliss-core, bliss-xray
  (the config and the process), bliss-tun's stack, and the desktop's
  `subscription.rs` by `#[path]`. Moving the shared parts into one crate is
  still ahead.
- **xray is an executable, not libXray**: `libxray.so` in the APK, run from
  the folder Android unpacks native libraries into, built with cgo against
  the NDK so names resolve through Android's resolver. That reuses the
  desktop's `Xray::spawn`, its readiness check and its leash (the
  parent-death signal, now on Android too) and needs no gomobile; libXray
  stays the option to measure against.
- **The socket speaks JSON lines, Rust on one side and Kotlin on the other**
  (`protocol.rs`, `Model.kt`), not postcard with UniFFI: the screens' process
  then loads no Rust at all. `tests/socket.rs` drives it as the screens do.
- **The service knows which TUN a frame is about** by a ticket `connect`
  answers (`Frame::ticket`, `holding`): a frame still in the socket from
  before the hand-over cannot read as "the engine let go".
- **IPv6 is routed in and refused** (an extra rule built in, not saved), so a
  program falls back to IPv4 instead of leaking around the tunnel. DNS goes
  to 1.1.1.1 inside the tunnel, through the server.
- **Not in it yet** (then; the second build added the first four): fake-IP
  (so domain and geosite rules see nothing),
  app rules and «мимо VPN», the rule editor, networks and presets by network,
  Автовыбор, the QR scanner, widgets, the work VPN, and «Только прокси». The
  tile and the notification with «Отключить» are in.
- **The real check** («Доступ»): beside the TCP ping, which says only that
  the server's port answers, a page is loaded through each server — a
  throwaway xray on a free loopback port, two `HEAD /generate_204` to
  `www.gstatic.com` on one connection, the second timed, four servers at a
  time, ten seconds each, on a tap and never on its own. A failed tunnel
  suggests a server that passed it before one that merely answers a ping.
  `a_server_nobody_answers_has_no_access` runs it against a host xray
  (`BLISS_XRAY`, ignored by default); a server that answers has been seen
  only on the phone.
- **First run on the user's phone, 1 November**: everything there worked.
  What they asked for after: the state word under the heart slid in and
  changed width with each word, which read as a jump (it crossfades in a
  box of one size now); a check that a server really takes you somewhere
  (above); and the empty top of Главная, which now holds the traffic card —
  rates, the chart, the delay, the session's totals — that sat as one line
  under the heart.

## The second build, 0.2.0, 1 November

Asked for after the first ran on the phone: the rules, the rule sets, and a
notification of bliss's own.

- **Sites by name: fake-IP** (`crates/tun/src/names.rs`). The service names
  `172.19.0.2` as the phone's DNS server, inside the TUN's /30; the stack
  answers there itself. An `A` query for a name with a dot gets an address
  from 198.18.0.0/15 (a pool of 16 384, the oldest name giving its address
  up past that), and a connection to it is decided and dialled by the name —
  so `domain` and `geosite` rules work through the TUN. Names of the local
  network (no dot, `.local`, `.lan`, `.home.arpa`…) are resolved for real on
  the phone's network; `AAAA`, `HTTPS` and `SVCB` get no records (IPv6 is
  not carried, and an `HTTPS` record's address hints would go around the
  name); anything else goes through the tunnel to 1.1.1.1. A direct route
  for a name resolves it on the real network, as direct routes always do.
  Android's «Частный DNS» set to a server's name bypasses all of this, and
  the rules then see addresses only: the screen says so.
- **Apps by package**: for each connection the stack asks
  `ConnectivityManager.getConnectionOwnerUid` through JNI
  (`android::owner_of` → `Native.ownerOf`, kept by name in
  proguard-rules.pro), off the runtime's threads, attached for the call and
  detached after. `app:`/`process:` rules match the package.
- **Правила**: «Приложения» — the launcher's apps (`<queries>` in the
  manifest), those with a way of their own first, a tap applying at once
  (`AppRoute`: the app's rule is made first in the list, changed, or taken
  away) — and «Список правил» with an editor sheet: a name, the action,
  conditions as chips added by kind (a site, an app from the list, a
  category from the rule sets, an address or port) or as the desktop's
  text, one to a line; up, down, delete with «Вернуть».
- **«Полностью мимо VPN»** is the service's list (`bypass.txt`, written by
  the screens): `addDisallowedApplication` for each, the interface built
  again when it changes.
- **Rule sets** (`geo.rs`): none ship in the APK. Three sources —
  Loyalsoldier (the default, ~28 MB), runetfreedom's lists for Russia
  (~100 MB: its geosite alone was 86 MB, which is why a file's cap is
  160 MB) and v2fly (~25 MB) — fetched through the tunnel when it is up,
  both files read as rule sets before either replaces what is there, then
  every rule compiled again. Fetched again by the week once here, when the
  tunnel comes up. The editor's category list is read from them only while
  it is open. `fetched_rule_sets_read_and_decide` read both real sources'
  files (1 807 and 1 830 categories) and decided `mail.google.com` by
  `geosite:google`; the fetch itself was not run here, since reqwest does
  not trust this machine's proxy.
- **The notification is a card of bliss's own** (`res/layout/note_*.xml`,
  `DecoratedCustomViewStyle`): the heart and the state in the accent, the
  server and its delay, a clock Android ticks by itself, and pills —
  «Пауза 5 мин», «Сервер», «Отключить». Opened, it shows the status card's
  night scene; paused, the bunny asleep in its cup and a count down.
  System fonts only: an app font that failed to load in SystemUI would be a
  bad notification and take the service with it. The pause is not offered
  under «Постоянный VPN» with its block, where it would mean no network.
- Not run on a phone before it was handed over: no emulator here.

## 0.2.1: what the phone showed

0.2.0 said «Подключено», carried nothing, and its «Отключить» did nothing.
The call back for apps was the cause: `Native.start` is a method of the
`Native` object, so the second argument JNI hands it is that object, and
0.2.0 kept it as the class to call `ownerOf` on. A release build does not
check (no CheckJNI), and the lookup on an object taken for a class hung
inside ART — every connection through the TUN waited on it for good, and
the hung thread, holding the runtime's lock, froze the service's own
Java threads, the one that takes «Отключить» among them. The class is now
looked up by name in `start`, and the stack waits 500 ms at most for an
owner before deciding without one (`an_owner_that_never_answers_is_not_waited_for`).
`a_lookup_through_the_stack_is_answered_from_the_pool` now carries a DNS
query through the real stack and back, on Linux.

The notification also stood as a dark box inside the system's card, its
«Отключить» pushed past the edge: it is colorized in the surface colour
now (a foreground service may), and its pills share the width.

## 0.2.2: the icon

0.2.1 worked on the phone. The launcher icon was a plain 96 px picture, which
Android sets on a white disc — in the launcher and in the header Android 12
and later put on every custom notification, which no app can take away. It
is an adaptive icon now: the mark lifted off the desktop icon's plate by
colour to alpha, inside the 66 dp safe circle, over a ground of the plate's
colours, and a monochrome layer for themed icons
(`android/make-icon.py`, from `crates/app/assets/bliss.ico`). The
notification's off pill reads «Выкл.»: «Отключить» was cut to «Отключ…».

## 0.2.3: countries

The user found `geoip:` missing from Правила. Two holes. The editor put the
countries after some 1 500 lists of sites, sixty at a time, so none ever
showed: «+ Страна» is a picker of its own now, a country by its flag and
Russian name (`geoipLabel`), the usual ones first. And on the phone every
site reaches the rules by name, which a `geoip:` rule never looks at: the
stack now learns the site's real address through the tunnel when a rule
that looks at addresses comes before the decision — at the DNS answer
already, ahead of the connection, when the table has any such rule — and
decides by name and address together. `a_site_by_name_meets_a_rule_by_address`
holds the stack to it; on both real rule sets an address in Russia meets
`geoip:ru`.

## 0.2.4: a rule that seemed not to work

The user's `geosite:reddit` → direct rule left Reddit going through the
server. The rule itself was right — run against Loyalsoldier's real
geosite with the phone's first rules, every Reddit host decided direct
(`an_editor_rule_decides_by_real_rule_sets`, ignored, `BLISS_GEO_DIR`) — so
the connections never met it as names, or never met the new table:

- **Open connections kept their way.** Rules apply to the next connection,
  and Reddit's app and the browser hold theirs for minutes. On the phone a
  rule change now ends the open connections the new table would send
  elsewhere, with a FIN, and the app reconnects the new way.
- **A name bliss never answered.** With Android's «Частный DNS» set to a
  server, or a browser's own DNS over HTTPS, a connection arrives as a bare
  address. Its name is read out of its first bytes now (TLS SNI, HTTP
  `Host:`), and QUIC to a bare address is refused so the program falls back
  to TCP, where the name can be read.

`a_bare_address_is_decided_by_the_name_it_says` carries a hand-made TCP
connection through the real stack: a ClientHello for `www.reddit.com` to a
bare loopback address, decided by a `reddit.com` rule, every byte echoed
back from the direct far end, and a FIN to the app once the rule goes. Its
first run failed on the test itself — the pipe standing in for the TUN
joined two packets into one read, which a TUN never does.

## 0.2.5: GitHub half loaded

Parts of GitHub's pages did not load with bliss on. The cause was 0.2.3's:
to let `geoip:` see sites, a site's real address was looked up through the
tunnel whenever an address rule came before the decision — and the phone's
first rule, «Локальная сеть», is one, as is the IPv6 refusal added last. So
nearly every new name waited on a DNS query through the server, up to five
seconds where the server lost the UDP. Now only a rule that could meet a
site's public IPv4 address counts; a failed lookup tries TCP, then is left
alone for a minute.

And «Блокировать QUIC через сервер», on by default, in Настройки: QUIC to
the server is refused so pages open over TCP — the user's own guess. A
connection whose name was read out of its own bytes now goes through the
server to the address the app chose, not by the name: an outer ECH name
resolves elsewhere.

## 0.2.6: QUIC as a rule

At the user's ask, the QUIC switch became a built-in rule, like «Локальная
сеть»: «QUIC — открывать по TCP», block, second in the list, with a
condition of its own, `protocol:quic` (xray's word), told by the flow's
first packet. Seen in the list, it can be switched off, moved, or put under
a rule that sends a site direct with its QUIC. A profile from before gets
it once, switched as the old setting stood; deleted, it stays deleted. The
desktop reads and writes the condition too (rule_text.rs, the table's
words), and its TUN tells QUIC the same way; the desktop window was
type-checked against stand-in libraries here, not run.

## 0.2.7: UDP and 443, as other clients write it

The user preferred the rule other clients use to `protocol:quic`: UDP and
port 443 → block. Rules here match on any one condition, so the network
became a limit rather than a condition, as in xray: a rule with `network`
decides only flows of that network, and its other conditions as before.
The editor chooses it apart from the conditions («Сеть: любая, TCP, UDP»),
the built-in rule is UDP with port 443, and 0.2.6's `protocol = ["quic"]`
is read as exactly that and rewritten on the phone's next start.

It also ends an error the user met on 0.2.6: ERR_QUIC_PROTOCOL_ERROR on
Google. QUIC was told by the Initial packet alone; a refused Initial ended
its flow, and the 0-RTT and later packets Chrome sent after it came in as
flows of their own, not Initials, and went through the server — Google
answered a handshake it never saw. UDP to 443 is refused whatever the
packet, and the bare-address refusal now knows any QUIC packet by its fixed
bit (`sniff::looks_like_quic`).

## 0.2.8: a refusal said aloud

0.2.7 still gave the user ERR_QUIC_PROTOCOL_ERROR on Google, with the rule
right. The refused datagrams were dropped in silence, and Chromium (Brave
here), which had seen QUIC to Google work through the server, waited on it
and gave up instead of going to TCP. A refused flow of datagrams is now
answered with an ICMP "port unreachable" from its far end, as sing-box
does, so the browser's socket fails at once and it opens the page over TCP.
ipstack owns the device and writes only TCP and UDP, so the answer goes out
through the device wrapper while ipstack waits on its next read.

## 0.2.9: Google closed on the user

With 0.2.8 Google answered ERR_CONNECTION_CLOSED, the QUIC rule on or off —
a TCP connection closed by bliss. Two ways it could, both mended without
the phone's log to tell which:

- **The pool's addresses were handed out in turn**, so after the tunnel's
  process restarted — every new build installed — they meant other names,
  while Brave still held its answers from before: google.com went to an
  address bliss did not know, and was closed, or knew as another site's. A
  name's address now comes from the name, the same after a restart, and a
  forgotten one is asked for its name and dialled by it.
- **IPv6 was routed in and refused by a rule**, and the stack accepts a
  connection before it is decided, so a refused one is an accepted one
  closed — which a browser takes as the site closing, without trying IPv4.
  The TUN now has no IPv6 address or route, and Android itself answers an
  app's IPv6 with "unreachable" at once (allowFamily's default).

On the user's phone 0.2.9 opened Google again in Brave, with the QUIC rule
switched off as well as on: the closed connections were never QUIC's. Which
of the two it was is not known.

## 0.3.0: QR codes, and a review

The add sheet reads a QR code (`Scan.kt`): the camera through CameraX, or a
picture from the gallery for a code sent as a screenshot, decoded by ZXing's
core — Apache 2.0, no Google Play services, nothing sent anywhere, and
light-on-dark codes too. A code that reads as a link is imported at once, as
«Вставить» does; anything else is said by kind and never quoted. The camera
is asked for on the first scan; refused, the gallery stays. The APK grew by
the camera libraries.

The user then asked for the whole code to be gone over, desktop and phone,
and for what turned up to be fixed. What was, each in a commit of its own:

- **One engine on the bridge port at a time** (`Port` in engine.rs). A stop
  only began the old xray's shutdown while the next start spawned its own,
  which found the port taken: «Полностью мимо VPN», «Продолжить» right after
  «Пауза» and a second server picked during a switch all failed the tunnel.
  Starts, switches and stops are turns now, in the loop's order. A server
  picked while the tunnel starts is switched to once it is up.
- **The engine's port asks for a login, on a port of its own per run.** Any
  app on a phone reaches loopback; xray's open SOCKS port let any of them go
  through the server around bliss's rules, or find the proxy by knocking.
- **The rule-set files are walked, not read whole**: every rule switch read
  all 85 MB of runetfreedom's geosite in the engine loop.
- **A site's address is asked once**, however many connections wait for it.
- **Commands reach the engine in order**, wait for it while its process
  starts, and name a server by a key as well as its place.
- The connections screen showed the oldest closed connections, never a live
  one; handshake times landed on neighbours after a refresh; «за сессию»
  never reset; a profile that did not read was written over at the next
  save; a refused `establish()` killed the tunnel's process; the socket read
  lines without end; an accept loop out of descriptors spun.

Left as found: the frame still carries the whole server list every second
while a screen is open.

## 0.4.0: following the phone

Asked for by the user after 0.3.0, as the design had them:

- **A frame is sent in parts.** A line holds only the top-level parts that
  changed since the screen's last one (`Sent`), the first after a hello
  holds all; the screen lays each over what it has (`applyPatch`), so the
  server list and the rules are parsed, and recomposed, only when they
  change — no longer with every tick of the numbers.
- **«Поделиться журналом»** on the journal tab: the engine writes
  journal.txt with every server address, SNI, subscription host, link,
  UUID and key taken out (report.rs), and the screen hands it to Android's
  share sheet. 0.2.9's Google is why: nobody could tell which change
  brought it back.
- **«Автовыбор»**: the fastest server a page loads through, the next one
  over the same TUN when it fails at a start, a switch, or when a screen
  comes on and nothing answers. Measured then and never on a timer.
- **Networks** (networks.rs): connect, stay off or leave it to the heart,
  and a preset, per network — mobile data, any Wi-Fi, a Wi-Fi by name with
  location permission, asked for on a tap. The service watches the default
  network with a callback; told to stay off, it keeps «Ждёт другой сети»
  so it sees the next one. A Wi-Fi behind a captive portal is connected
  once signed into. None of it works with bliss off and no screen open:
  no process of bliss's is there to see the network.

The desktop got the name out of the connection under TUN the same day
(AGENTS.md §9).

Build: `ANDROID_NDK_HOME=… android/build-native.sh`, then
`gradle assembleRelease` in `android/` (AGP 8.13, Gradle 8.14, JDK 17+).

## 0.5.1: a server for each network, and stars

Asked for by the user after the first release built by GitHub Actions.

- **A network can name a server**: a server by its key, or «Автовыбор»
  (`NetworkRule::server`, `"auto"`), taken as the phone moves onto the
  network the way a tap in the list takes it — switching a tunnel that is
  up, said in a line. A server deleted since is passed over, and the sheet
  says it is gone (`server_name` none). The sheet picks it from a list
  that opens under «Сервер», starred servers first.
- **Stars**: `favorites` in android.json, by key, so a refreshed list keeps
  them; a star on a server that is gone is dropped as the next one is set,
  and past 256 the oldest (`a_network_takes_its_server_and_stars_have_a_cap`,
  seen failing with the cap taken out). The list shows them under
  «Избранное» above everything and not again in their group; the quick
  sheet puts them first.
- **No more «сердечко» in words.** The networks' choices read «Решает
  сердечко», which the user needed many tries to understand: the heart is
  the main button, and nothing on screen calls it that. They are now
  «Включать bliss», «Выключать bliss», «Не включать и не выключать», with a
  summary of what a network does («включаю · сервер X · пресет «Дома»», or
  «ничего не делаю»), and the card's text says in one sentence what it does
  and that it works until bliss is switched off by its button. The main
  screen says «нажмите, чтобы подключиться».

## 0.5.2: the server in use, kept

The user found the server changing between restarts. `selected` was
written on a tap alone; «Автовыбор», a failover and a network's rule moved
the server without it, so the next start of the tunnel's process came back
on the one picked by hand before. Every change now goes through `remember`,
and a deleted server takes the file with it
(`the_server_in_use_outlives_a_restart`). The desktop learnt the same, and
stars, in the same release (AGENTS.md §5).

**And the heart, overruled.** The user found bliss switching on and off
in a blink when the heart was pressed while the phone moved between
networks. Moving, Android calls back several times — the old network
lost, the new one, at times the old one again — and each was said to the
engine at once, a move with its rule acted on; a heart pressed off in
between was switched back on by the next «Включать bliss», and a second
start could land on a first still under way. Now the service says a
network once it has held for 1.5 s (`SETTLE_MS`), starts nothing while a
TUN it handed over is unanswered (`starting`), and a heart pressed off —
or the tile, or the notification's «Выкл.» — holds every rule's «on» back
until the person switches bliss on again (`held_off`,
`the_heart_switched_off_outweighs_a_networks_rule`, seen failing without
it). The network's own «off», a pause and another VPN taking over do not
hold it (`Native.disconnect(byHand)`). Not reproduced on a phone: the
sequence is read from the code.

## 0.5.3: the Wi-Fi's name under the tunnel

The user found the networks card naming their Wi-Fi with the tunnel off
and a bare «Wi-Fi» once it was on. The service watched the default
network alone, and with a VPN up Android can hand that out as the VPN
itself, which reports Wi-Fi among its transports and no Wi-Fi's details;
and an app out of sight without background location gets the name
redacted. Now every network that is not a VPN is watched as well, a VPN
as the default stands for the best of them (a working Wi-Fi, then a
cable, then mobile data), and a name once read is kept for as long as
that network is connected. Not seen on a phone: which of the two the
user met is not known, and both are covered.

**And with the phone in a pocket.** Android gives a Wi-Fi's name to an
app in sight, or to one allowed location «всегда»; the tunnel's service
is `specialUse`, so out of sight every Wi-Fi was nameless and met the rule
for any Wi-Fi rather than its own. The networks card now asks for
`ACCESS_BACKGROUND_LOCATION` once Wi-Fi names are allowed, in words that
say why, and sends a person whom Android will not ask again to bliss's
own settings page. And a network is read again whenever the screens come
up (`TunnelService.rewatch`, over the binding they already hold — a new
watch is answered at once with every network as it is): Android says a
network only when it changes, so a name allowed or shown later never
arrived. `reportNetwork` used `startService`, which with the tunnel off
left a started service behind; so did `rebuild`. A command that leaves
nothing running now stops it (`stopSelf(startId)`), and the binding keeps
it while the screens are up.

## 0.5.4: a first run, walked through

At the user's ask, the code read in the order a new person meets it — the
first launch, the first server, the consent, the rules, the apps, the
settings, the traffic screen, the tile. What was found:

- **The first line ever said was never shown.** The snack took the first
  toast it saw for one from before the screen opened; on a first run that
  was «Добавлен …». The engine's first frame now marks what came before.
- **A server already listed read as «no link here»**, and so did a full
  list: a paste now says «Этот сервер уже есть в списке», «Все эти серверы
  уже есть», or that the list is full (`a_paste_says_what_it_found`). The
  first of new servers is the one chosen, not the last.
- **«Полностью мимо VPN» did not show in the apps list** until a rule
  changed: the list read the bypass file on a rule change alone; it now
  follows `Bypass.version`.
- **A site typed in the rule editor and not added** kept «Сохранить» grey,
  or was dropped from a rule that had another condition; it is saved with
  the rest. A `network:` line written in «Текстом» no longer gets a second
  one from the switch.

## 0.5.7: the notification without a box

Android 12 and later start a decorated notification's content under the
app's name and keep the column under its icon, so bliss's card stood as a
box right of the notification's middle (seen on the user's Pixel 7).
Asked for by the user: in a dark shade the card has no ground of its own
(`drawable-night/note_card.xml` is clear, `note_fade` too) and the scene
fades out by itself — `note_scene_night.png`, the status card's picture
made opaque in its body and faded to nothing over the left 42%, the top
30%, the bottom 22% and the right 5%. Keyed out by colour instead, the
dark forest went see-through and took the shade's red. A light shade keeps
the card, or its light words would stand on white. Remake the PNG when
status-card.jpg changes.

**0.5.8–0.5.9.** With the box gone nothing seemed to move: the content
still started under the app's name, 73dp in, with 16dp on the right. The
words now start there and the right keeps as much free (`note_end`), which
sets the block in the notification's middle, and the scene fades less.
And the light shade's card went too: the service colorizes its
notification (`setColorized` with the surface colour), so its ground is
bliss's dark plum in a light shade as in a dark one — the user's Pixel 7
in a light theme showed the card as a box inside it. One look for both.

## 0.5.11: when Android does not give the VPN

A user on a Samsung with Android 17 saw no consent dialog at all: the heart
did nothing. bliss swallowed a consent that did not come. Android closes its
dialog by itself while another app holds «Постоянный VPN» (Samsung's own
Secure Wi-Fi is one), and offers none in a work profile or Secure Folder.
Now a consent that did not come opens `RefusedSheet`: closed within 1.5 s,
or with no dialog to open (`ActivityNotFoundException`), it says the dialog
never stayed and why that happens, with a button to Android's VPN settings
and another try; a refusal pressed by hand gets the short word. Not seen on
that phone: the cause there is the likeliest one, not a measured one.

## 0.5.14: datagrams are not a SOCKS association

An article of 7 April 2026 on VLESS clients says any app on a phone can ask
a client's local proxy where the tunnel comes out, and that UDP gets past a
login. The first half was closed from the start (a login and a port that
differs every run, 0.2.x). The second is true of xray 26.3.27, checked
here: with a SOCKS UDP association open — any UDP flow of the TUN — a
datagram in SOCKS's form sent to the proxy's port by another program is
carried out through the server with no login. So the SOCKS inbound now
serves TCP only (`udp: false`), and datagrams go over a Trojan inbound
beside it: UDP inside the TCP stream, behind the run's password, no UDP
socket for a stranger to write to (`trojan_udp.rs`, in `bliss-core`, the
client end; `bliss_xray::datagrams_over_trojan`). Seen with a real xray:
the datagram of a stranger gets nothing at either port while a flow is in
the air, a wrong password gets nothing, and the test fails with the UDP
left on. The geo files' and subscriptions' fetch go through the SOCKS port
as before, with its login.

What the article also asks of clients and bliss does not do: nothing runs
xray's API (no `HandlerService`, no `StatsService`), and the port is not
one of the well-known ones; a program that scans loopback still finds a
SOCKS port that wants a login, and a Trojan port that says nothing.

## 0.5.15: an app outside the VPN cannot borrow its interface

The user ran RKNHardering in the Shelter work profile that holds their
Russian apps, beside bliss 0.5.14 in the main one. From there it saw no
`TRANSPORT_VPN`, and the SOCKS port it found asked for a login; but it bound
a socket to `tun0` by name (`SO_BINDTODEVICE`, which Android lets any app
do), sent a request to an IP checker through it, and read the server's
address off the answer — its hard verdict. Android routes an app into the
VPN by its uid; a socket bound to the interface goes in whatever its uid.
So the engine now asks whose each connection is (it already did, for app
rules) and closes those of another user of the phone — `ownerOf` says
`foreign:<uid>` when the uid's user differs from bliss's — and of the apps
«мимо VPN» in `bypass.txt`, which can do the same. A connection whose owner
the system does not name is carried, as before. Run again from Shelter with
0.5.15 the same evening, RKNHardering found no way through the tunnel: no
«VPN network binding», no VPN channel among the addresses it saw, and the
server's address nowhere in the report; its verdict went from DETECTED
(R1, a bypass confirmed) to NEEDS_REVIEW, on `tun0` and the routes alone.
So `getConnectionOwnerUid` does name a work profile's sockets to the VPN's
app, at least on the user's phone.

What the same report found and nothing in an app can change: `tun0` in
`getifaddrs()`, `if_indextoname`, `/proc/sys/net/*/conf` and the routing
table; and a default route through it beside Wi-Fi's.
