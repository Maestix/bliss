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
