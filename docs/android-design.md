# bliss on Android — the design

A design, not code. Asked for on 31 October 2026: the desktop window carried
to a phone, "simple and sweet", nothing it can do left behind, and nothing in
it that grates with use. The mockups are a canvas of fourteen artboards on
claude.ai (`bliss для Android`, private to the user until shared); most of
them are clickable — the heart connects, presets and switches move, a server
can be deleted and given back, the tabs lead to each other. This file is
what the canvas cannot say: why each piece is where it is, and what Android
demands of bliss underneath before the design can be built.

Nothing here has been implemented, and nothing here has been checked on a
device or an emulator. Section numbers (§9) and rule numbers (rule 30) are
AGENTS.md's; the rules are those of its §6.

## The look is the window's

The tokens are `theme.css`'s, unchanged: plum grounds (`#140f1d`, cards
`#1e1629`), soft pink for what is on, chosen or tappable (`#ee82c4`), violet
for what is on its way (`#a98ce8`), mint for direct (`#72cf9e`), red for
blocked and failed (`#e56b6f`), sky blue for the work VPN (`#7eb3ee`), the
lavender neutral ramp for everything else. Comfortaa at 600, headings at 700.
The night scene, the cat on its cloud, the bunny in its cup and the mark are
the desktop's own files, composited on `--color-surface` as they already are,
so they sit only on cards of that colour.

What changes is scale, for fingers:

| | desktop | Android |
|---|---|---|
| card radius | 8px | 20px |
| tile radius | 4px | 14px |
| dialog / sheet | 14px | 28px top corners |
| button | 36px, outline | 48px, outline on a pink tint |
| chip, rule switch | 26px, 30×18 | 40px, 48×28 |
| smallest target | — | 44px |
| gutter | 12–20px | 16px; 12px between cards, as on desktop |

Primary buttons stay the desktop's: accent text and hairline on a faint tint,
never a solid pink slab. Dark only — Material You's wallpaper colours would
take the program's look away, and it has no light theme on desktop either.
What Android draws itself (the notification, the quick settings tile, the
VPN consent dialog) is left in Android's style with bliss's accent where the
system allows one.

Comfortaa still has no tabular figures (§6 rule 11 of AGENTS.md): the clock
under the ring and every rate need the same `.figure` boxes, or they jitter on
a phone exactly as they did in the window.

## Navigation: six rail items become five tabs

The rail had Подключение, Логи и трафик, Серверы, Подписки, Маршрутизация,
Настройки. The bottom bar has **Главная, Серверы, Правила, Трафик,
Настройки**. Subscriptions are not a tab: the server list already groups a
subscription's servers under its name, so on the phone the group header *is*
the subscription — its usage bar, its age, and a «⋯» that opens its sheet
(rename, new address, copy the address, refresh, delete). One place for one
thing (rule 30).

## Главная

From the top: the mark and the name, with the work VPN's chip on the right
(where it was beside the server's name); the night scene with the ring and
its heart, the state word under it and one line — «В сети 00:12:44»,
«поднимаю туннель до AMS-2…», «нажмите на сердечко»; the server card (name,
how it hides, ping) whose tap opens the quick server sheet; the preset chips;
the traffic card (rates, the chart, latency, connections, session total),
whose tap opens Трафик.

- The heart is the only switch for the tunnel and sits where a thumb rests.
  Its four tones are the desktop's `ON`, `BUSY`, `IDLE`, `FAILED`. It does
  not breathe while connected — a glow animated for hours costs battery for
  nothing; only «Подключение» pulses, and that ends.
- The server's name is said once, on its card. The status card on desktop
  held name, state, address and fingerprint together; on a phone the hero is
  the state and the card below is the server.
- **The TUN switch under the ring is gone.** On Android the VPN service *is*
  the TUN, and the heart starts it. The choice it stood for — only what is
  pointed at the ports, or the whole machine — is the first card in
  Настройки: «Весь телефон» (VPN service) or «Только прокси» (127.0.0.1:10808
  for apps where a proxy is set by hand, no VPN key in the status bar).
- A failure is said in a red note under the hero, with the way out on it:
  «Ещё раз» and «Другой сервер». No network is violet, not red, and says
  bliss will reconnect by itself.

## The tray becomes four things

The desktop's tray menu (state, servers, presets, proxy mode, open, quit)
does not exist on a phone. Its jobs go to:

- **The quick server sheet**, from the server card: search, servers grouped
  by subscription with ping, the current one ticked, and at the foot
  «Отклик» and «Переподключить» — the old «Перезапустить туннель». The
  bunny sits on the sheet's edge, as it sits in the tray menu's corner.
- **The notification**, which Android requires of a running VPN service: a
  silent, low-importance channel; state and server, rates, preset, the work
  VPN; «Отключить» and «Сменить сервер». Rates change at most every 2 s and
  only while the screen is on; a switch in Настройки takes them out.
- **A quick settings tile** (`TileService`): tap connects or disconnects,
  long press opens bliss.
- **Widgets**: a 2×2 with the scene and the heart, and a 4×1 with the server,
  the rates and the preset. They redraw on a change of state and otherwise
  at most once a minute.

And launcher shortcuts on the icon (Подключить, Сменить сервер, Добавить из
буфера), which cost nothing and save opening the app.

## Серверы

Own servers first, then each subscription as a group. The current server has
a pink tint and a small filled heart; ping keeps the desktop's three bands
(under 120 ms mint, under 300 ms violet, slower grey) plus «нет» in red and
«UDP» for servers a TCP handshake cannot measure. A tap switches — the
snackbar says the tunnel restarts — and «⋯» offers copy, share and delete.

- **Checking every server is pulling the list down**, and the pulse button in
  the header for those who do not know the gesture. The «Действия» tab's
  other entries went where they are used: refresh in the subscription's
  sheet, the work folder became «Поделиться журналом» on Трафик, the system
  proxy has no Android counterpart (an app cannot set it).
- **«Добавить»** is one sheet for a server link, a subscription address and
  an `.ovpn`: «Вставить» and «QR-код» as two large tiles, a field for typing,
  and a verdict under it in words by kind — «Сервер VLESS — ляжет в
  «Свои»», «Подписка — загружу список», «tuic:// движок xray не знает» —
  never quoting the link back. Links also arrive by «Поделиться» from any app
  and by `vless://`-style links opened in a browser; both open this sheet
  filled in.
- The subscription's address is shown as scheme and host only, its field
  starts empty with «вставьте новый, чтобы заменить», exactly as
  `SubscriptionEditor` does: the token is in the path.

## Правила

The routing screen nearly unchanged: preset chips (a violet dot and
«Сохранить» once a switch moved since), the address check as a field with
its answer under it, the warnings with a button that fixes them where one
can, the rules with their switches, the fallback as two chips.

- **Order is changed with the grip on the right**, by dragging, instead of
  two arrows per row.
- **The editor is a sheet**, never taller than the screen, its buttons above
  the keyboard (rule 15). Conditions are chips, added by kind — «Сайт»,
  «Приложение» (installed apps with their icons), «Страна или список»,
  «Адрес или порт» — and «Написать текстом» turns them into the desktop's
  one-per-line box with the same prefixes (`rule_text.rs` already reads
  `app:`). Typing `geosite:category-ads-all` on a phone keyboard is the
  thing this avoids.
- **«Полностью мимо VPN»** appears for a direct rule that names apps. A
  direct rule still runs the app through the VPN interface, and many
  banking apps refuse to work while they see a VPN at all; excluding the app
  from the service (`addDisallowedApplication`) is the only cure. It is also
  the one rule change that cannot be applied live — the interface has to be
  re-established, about a second — and the switch says so.

## Трафик

Two tabs, «Соединения» and «Журнал». Connections are rows of app icon, host,
app name, bytes and the route pill; a tap — not a right click, not a long
press — opens the connection's sheet: the numbers, and four actions for a
rule by its site and four by its app, each opening the editor with the
condition filled in, as the desktop's menu does. The log keeps its level
colours (`logs.rs`), copies a line on tap, and is shared from the header
along with the table.

## Настройки

What goes through bliss (whole phone or only proxy) with a link to the apps
that bypass it; starting with the phone, and Android's own always-on VPN
behind «Открыть» — Android can then start bliss itself and block traffic
around it, which no setting inside bliss can do; the work VPN; the
notification's rates; and the bunny with the version and the engine's.

## So it does not grate

- **No «Вы уверены?» anywhere.** Deleting a server, a subscription or a rule
  happens at once, with «Вернуть» in a snackbar for six seconds. The desktop
  asks for a second click in place; on a touch screen a second tap lands on
  the same spot by accident far too easily.
- **The clipboard is read only when asked**: Android 12 and later tell the
  person, in a banner, whenever an app reads what another app copied, so
  bliss never looks on its own when it opens. What it copies — a link, a subscription address — is marked
  sensitive (`ClipDescription.EXTRA_IS_SENSITIVE`), so the system's
  clipboard preview hides it.
- **Sheets close themselves** once a choice is made, and to a swipe down and
  the back gesture; the back gesture closes a sheet before it leaves a tab.
- **Nothing is hidden behind a gesture.** Every long press and swipe has a
  visible twin: «⋯» on servers and subscriptions, a tap on a connection, the
  grip on a rule, the pulse button beside pull-to-refresh.
- **Asked before Android asks.** The first tap on the heart opens a sheet
  that says the VPN consent dialog is coming and which button to press, and
  only «Понятно» calls `VpnService.prepare`. The same for notifications on
  Android 13.
- **Quiet by default**: the notification makes no sound, the ring animates
  only while connecting, the chart glides only while it is on screen (rule
  22's cost on desktop is a battery's on a phone).

## What Android asks of bliss underneath

These are the parts of the design that are promises about the engine:

- **Domain rules need names.** A VPN service sees packets, and a packet has
  only an address (§9: `domain` and `geosite` rules never match TUN
  traffic). On a phone nearly everything goes through the TUN, so without
  names most rules would do nothing. Android gives the answer desktop lacks:
  the service sets its own DNS server inside the tunnel (`addDnsServer`), so
  every lookup comes to bliss and fake-IP works. It, or sniffing the name out
  of TLS and HTTP, has to exist before Правила can mean on Android what they
  mean on desktop.
- **Programs are apps.** `ConnectivityManager.getConnectionOwnerUid`
  (Android 10, API 29) names the UID behind a connection; the UID names the
  package. Rules keep `app:` with package names and show label and icon.
  API 29 is therefore the floor.
- **One VPN at a time.** Android runs a single VPN service, so the work VPN
  cannot be the official OpenVPN app beside bliss: OpenVPN has to run inside
  bliss's own service, its packets carried by the same TUN and routed by
  «через VPN» rules as on desktop. The libraries that do that (OpenVPN 3
  Core, ics-openvpn) are under the AGPL and the GPL; which one, and what that
  means for bliss's licence, is a decision before any code.
- **The engine is in the package.** No «Установить движок»: xray ships with
  bliss (as `libxray` built with gomobile, or as an executable among the
  APK's native libraries), and updates with the app.
- **No system proxy.** An app cannot set Android's; «Только прокси» is the
  honest version of the desktop's three modes.
- **The work VPN's DNS, fake-IP and «мимо VPN» meet in one place** — the
  service's `Builder` — and a change to the last re-establishes it. Every
  other rule change is applied live, as on desktop.

How the window would get there is open. Dioxus can target Android with the
system WebView, which would carry `app.css`, the theme and most of `ui.rs`'s
habits over (and §6's WebView rules with them); the VPN service, the tile,
the widgets and the notification are Android components in Kotlin either
way, talking to the Rust core over JNI.

## Not designed yet

A landscape and tablet layout; TalkBack's reading order (every control on
the canvas is a real button with a label, but nobody has listened to it);
the subscription editor's error states; the rule editor's site, list and
address adders (only the app picker is drawn); and what the heart does while
the always-on VPN is locked down by Android.
