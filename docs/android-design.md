# bliss on Android — the design

A design, not code. Asked for on 31 October 2026: the desktop window carried
to a phone, "simple and sweet", nothing it can do left behind, and nothing in
it that grates with use. The same evening the user asked for a second pass
that was free to leave the window behind where a phone is better served
otherwise; that pass is what this file describes, and its departures from
the desktop are listed at the end with their reasons.

The mockups are a canvas of seventeen artboards on claude.ai (`bliss для
Android`, private to the user until shared). Most are clickable: the heart
connects and shows its steps, presets and switches move, a server can be
deleted and given back, an app's route applies at a tap, the tabs lead to
each other. This file is what the canvas cannot say: why each piece is where
it is, and what Android demands of bliss underneath before the design can be
built.

Nothing here has been implemented, and nothing here has been checked on a
device or an emulator. Section numbers (§9) and rule numbers (rule 30) are
AGENTS.md's; the rules are those of its §6.

## What a phone is used for

The window is open on a desk for hours; a phone is taken out to do one
thing and put away. In order of how often: switch the tunnel on or off;
change a server that has gone slow; move between home and outside, which a
phone does by itself several times a day; now and then send one app another
way; rarely add a server, edit a rule or read the log. The layout follows
that order, and one hand: what is done often sits where a thumb reaches, in
the lower half; what is read sits above it; what is rare is one level down.

## The look is the window's

The tokens are `theme.css`'s, unchanged: plum grounds (`#140f1d`, cards
`#1e1629`), soft pink for what is on, chosen or tappable (`#ee82c4`), violet
for what is on its way (`#a98ce8`), mint for direct (`#72cf9e`), red for
blocked and failed (`#e56b6f`), sky blue for the work VPN (`#7eb3ee`), the
lavender neutral ramp for everything else. Comfortaa at 600, headings at 700.
The night scene, the cat on its cloud, the bunny in its cup and the mark are
the desktop's own files.

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
never a solid pink slab. Dark only. What Android draws itself (the
notification, the quick settings tile, the VPN consent dialog) is left in
Android's style with bliss's accent where the system allows one.

- **The scene is the sky of the whole home screen** now, not a card: full
  width, faded into the ground at both ends, the moon top right. The file
  was cut for a wide card and is stretched 1.44× here; a portrait cut of the
  generated original, at the phone's density, is wanted before it ships
  (the originals are not in git, §6 rule 10).
- The cat and the bunny were composited on `--color-surface`, so they sit
  only on cards of that colour, as on desktop.
- Comfortaa still has no tabular figures (rule 11): the clock under the ring
  and every rate need the same `.figure` boxes, or they jitter on a phone
  exactly as they did in the window.

## Navigation: four tabs

**Главная, Серверы, Правила, Настройки.** The rail's six became four:

- Subscriptions are not a tab. The server list already groups a
  subscription's servers under its name, so on the phone the group header
  *is* the subscription — its usage bar, its age, and «⋯» for its sheet
  (rename, new address, copy the address, refresh, delete). One place for
  one thing (rule 30).
- **Трафик is not a tab either.** Connections and the log are for finding
  out why something misbehaves, which is rare on a phone; a tab for them
  took a fifth of the bar every day. It is a screen pushed over the tabs,
  with a back arrow, opened by the rates line on Главная and from
  Настройки. Four tabs also give each target a quarter of the width.

## Главная

Top to bottom: the mark and the name, the network the phone is on and the
work VPN's chip; the sky; the ring with its heart, the state word and one
line under it; the rates as a line that opens Трафик; the preset chips; the
server card.

- **The heart sits at the middle of the screen**, a 156px ring whose centre
  is about 400px from the top — reachable with the thumb of the hand holding
  the phone. The first pass had it at the top of a card, under the header.
  Its four tones are the desktop's `ON`, `BUSY`, `IDLE`, `FAILED`.
- **A start shows its steps**: «движок · сервер · проверка», each lit in
  violet as it is done, the current one blinking. On a phone a start can
  take seconds over a weak network, and a spinner says nothing about which
  of them is slow.
- **A failure offers the next server at once**: «AMS-2 не отвечает уже
  10 с» and under it «Подключить FRA-1 · 68 мс», the best answer the last
  ping sweep gave. A server that fails on a phone is most often one that
  this network blocks, and «try again» is the wrong default there.
- **The server card is at the bottom**, over the tab bar, with a chevron up:
  it opens the quick server sheet. The traffic card of the first pass is
  gone; its numbers are the one line «↓ 2,4 МБ/с · ↑ 186 КБ/с · 14 соед.»
  and the chart lives on Трафик.
- **The network chip** («Дом») opens what bliss does on this network
  (below). A preset that the network chose carries a small Wi-Fi mark on its
  chip; tapping another preset is a manual choice that lasts until the
  network changes.
- The TUN switch under the ring is gone: on Android the VPN service *is* the
  TUN, and the heart starts it. «Весь телефон» or «Только прокси» is the
  first card in Настройки.

## Networks: bliss follows the phone

Every network has three settings: connect on it, don't, or leave it to the
heart; and which preset to apply there. Home Wi-Fi might connect with
«Дома», the office Wi-Fi not connect at all, mobile data and unknown Wi-Fi
connect with «В дороге». A change of network is said in a snackbar with a
way back for this time only — «Мобильная сеть — пресет «В дороге»» /
«Оставить «Дома»».

- Telling Wi-Fi from mobile data needs nothing. **Telling one Wi-Fi from
  another needs location permission**: Android gives an app the network's
  name (SSID) only with `ACCESS_FINE_LOCATION`, and only with location
  services on. bliss asks for it the first time someone names a network,
  says that it never reads where the phone is, and without it offers «any
  Wi-Fi» and «mobile» only. Asking at install would look like exactly what
  a VPN must not look like.
- **A captive portal is waited out.** When the network reports one
  (`NET_CAPABILITY_CAPTIVE_PORTAL`), bliss does not raise the tunnel into a
  network that leads nowhere: it says «Wi-Fi «Кофейня» просит войти» with
  a «Войти» that opens Android's sign-in, and connects once the network is
  validated.

## The tray becomes four things

The desktop's tray menu (state, servers, presets, proxy mode, open, quit)
does not exist on a phone. Its jobs go to:

- **The quick server sheet**, from the server card: «Автовыбор» first, then
  the three servers used last, then «Все серверы» into the tab. A list of
  dozens with a search box was the first pass; nobody scrolls dozens to
  change servers on a bus. At the foot «Отклик» and «Переподключить» — the
  old «Перезапустить туннель». The bunny sits on the sheet's edge.
- **The notification**, which Android requires of a running VPN service: a
  silent, low-importance channel; state, server, rates, network and preset;
  «Пауза 5 мин», «Отключить», «Сервер». The pause is for an app or a page
  that will not work through a tunnel for a moment; it brings the tunnel
  back by itself, which a «switch off and remember to switch on» never
  does. Under Android's «block connections without VPN» there is no pause,
  and the action is not shown.
- **A quick settings tile** (`TileService`): tap connects or disconnects,
  long press opens bliss.
- **Widgets**: a 2×2 with the scene and the heart, and a 4×1 with the server,
  the rates and the preset; redrawn on a change of state and otherwise at
  most once a minute.

And launcher shortcuts on the icon (Подключить, Сменить сервер, Добавить из
буфера).

## Серверы

«Автовыбор» on top, then own servers, then each subscription as a group. The
current server has a pink tint and a small filled heart; ping keeps the
desktop's three bands (under 120 ms mint, under 300 ms violet, slower grey)
plus «нет» in red and «UDP» for servers a TCP handshake cannot measure. A tap
switches — the snackbar says the tunnel restarts — and «⋯» offers copy,
share and delete.

- **Автовыбор** takes the fastest server and leaves one that stops
  answering. It measures on a start, on a failure and when the screen comes
  on — never on a timer in the background, which on a phone is battery and
  mobile data spent on numbers nobody looks at.
- **Checking every server is pulling the list down**, and the pulse button
  in the header for those who do not know the gesture.
- **«Добавить»** is one sheet for a server link, a subscription address and
  an `.ovpn`: «Вставить» and «QR-код» as two large tiles, a field for typing,
  and a verdict under it in words by kind — «Сервер VLESS — ляжет в
  «Свои»», «Подписка — загружу список», «tuic:// движок xray не знает» —
  never quoting the link back. Links also arrive by «Поделиться» from any app
  and by `vless://`-style links opened in a browser; both open this sheet
  filled in.
- The subscription's address is shown as scheme and host only, its field
  starts empty with «вставьте новый, чтобы заменить», as
  `SubscriptionEditor` does: the token is in the path.

## Правила: apps first

Two tabs, **«Приложения»** first and **«Список правил»** second. On a phone
the question is nearly always "send *this app* another way", and the
desktop's answer — compose a rule, find the program's name, type it — is the
wrong shape for it.

- «Приложения» lists the apps with their icons: those with a rule of their
  own first, the rest by how much they used this session, each «по правилам
  сайтов» until given one. A tap opens the app's sheet: its traffic, and five
  rows — «Как решат правила сайтов», «Через прокси», «Напрямую», «Не пускать
  в сеть», «Через рабочий VPN». **A tap applies**: the sheet says «сохранено —
  правило №7 «YouTube»» and there is no Save button, because there is
  nothing else to fill in. Choosing «Как решат правила сайтов» deletes the
  rule.
- **These are the same rules**, seen from the app's side: the app's choice is
  an `app:` rule in «Список правил», editable and reorderable there like any
  other. Two lists of the same truth would drift; one list with two views
  cannot.
- **«Полностью мимо VPN»** appears for «Напрямую». A direct rule still runs
  the app through the VPN interface, and many banking apps refuse to work
  while they see a VPN at all; excluding the app from the service
  (`addDisallowedApplication`) is the only cure. It is the one rule change
  that cannot be applied live — the interface has to be re-established,
  about a second — and the switch says so. bliss sees nothing of such an
  app afterwards, and its rows say «bliss его не видит» instead of bytes.
- «Список правил» is the routing screen: preset chips (a violet dot and
  «Сохранить» once a switch moved since), the warnings with a button that
  fixes them where one can, the rules with their switches and grips, the
  fallback as two chips, and «Правило» as a floating button in the thumb's
  corner. The address check is behind the search icon in the header: a tool
  for finding out, not something to keep on screen.
- **The editor is a sheet**, never taller than the screen, its buttons above
  the keyboard (rule 15). Conditions are chips added by kind — «Сайт»,
  «Приложение», «Страна или список», «Адрес или порт» — and «Написать
  текстом» turns them into the desktop's one-per-line box with the same
  prefixes (`rule_text.rs` already reads `app:`).

## Трафик

A screen over the tabs: the chart that left Главная, the session's totals,
and three tabs. **«Приложения»** is first — on a phone "which app is eating
my data" is the question — with bytes per app and where they went;
«Соединения» are the desktop's table as rows, each opening its sheet with
four rule actions by site and four by app; «Журнал» keeps its level colours
(`logs.rs`) and copies a line on tap. The header shares the table or the
log.

## Настройки

What goes through bliss (whole phone or only proxy); the networks; starting
with the phone, and «Не пускать трафик мимо» behind «Открыть» — Android's
always-on VPN with its block, which no setting inside bliss can provide; the
work VPN; the notification's rates and the vibration; Трафик; the bunny with
the version and the engine's.

## So it does not grate

- **No «Вы уверены?» anywhere.** Deleting a server, a subscription or a rule
  happens at once, with «Вернуть» in a snackbar for six seconds. The desktop
  asks for a second click in place; on a touch screen a second tap lands on
  the same spot by accident far too easily.
- **What can apply at once, applies at once**: an app's route, a network's
  setting, a preset. Save buttons are left where something is typed.
- **The clipboard is read only when asked**: Android 12 and later tell the
  person, in a banner, whenever an app reads what another app copied, so
  bliss never looks on its own when it opens. What it copies — a link, a
  subscription address — is marked sensitive
  (`ClipDescription.EXTRA_IS_SENSITIVE`), so the system's preview hides it.
- **The phone can stay in the pocket**: a light tick when the heart is
  tapped, a soft double one when the tunnel is up
  (`HapticFeedbackConstants.CONFIRM`), and a switch to turn both off.
- **Sheets close themselves** once a choice is made, and to a swipe down and
  the back gesture; the back gesture closes a sheet before it leaves a tab.
- **Nothing is hidden behind a gesture.** Every long press and swipe has a
  visible twin: «⋯» on servers and subscriptions, a tap on a connection or an
  app, the grip on a rule, the pulse button beside pull-to-refresh.
- **Asked before Android asks.** The first tap on the heart opens a sheet
  that says the VPN consent dialog is coming and which button to press, and
  only «Понятно» calls `VpnService.prepare`. The same for notifications on
  Android 13, and for location when a first network is named.
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
  API 29 is therefore the floor. The apps view needs the installed apps,
  which since Android 11 an app sees only if it declares them: a `<queries>`
  entry for launcher activities (`ACTION_MAIN` with `CATEGORY_LAUNCHER`)
  shows every app a person can open, which is every app worth a rule,
  without `QUERY_ALL_PACKAGES` and the justification Google Play asks for
  it.
- **The network is watched, not polled**: a `NetworkCallback` on the default
  network says when it changes, whether it is Wi-Fi or mobile, whether it
  has a captive portal and when it is validated. The SSID needs location
  permission (above).
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

## Where the phone leaves the window behind

Asked for in the second pass, each for a reason the phone has and the desk
does not:

| desktop | Android | why |
|---|---|---|
| the ring at the top of the status card | the ring at mid-screen, the scene as the sky | one hand, thumb reach |
| six screens in a rail | four tabs, Трафик pushed over them | daily use is three of them |
| a traffic tab with the chart | a line of rates on Главная, the chart on Трафик | read at a glance, studied rarely |
| rules as a list | apps first, the list second, both one set of rules | "this app another way" is the phone's question |
| a rule saved from an editor | an app's route applied at a tap | nothing to type |
| presets chosen by hand | presets and connecting chosen by network | a phone changes networks by itself |
| «Ошибка» and the log | the start's steps, and the next server offered | a blocked server is the common failure on a phone |
| the full list in the tray menu | Автовыбор and the three last servers | nobody scrolls dozens on the move |
| — | pause for five minutes; waiting out a captive portal; vibration | things a phone meets and a desk does not |

## Not designed yet

A landscape and tablet layout; TalkBack's reading order (every control on
the canvas is a real button with a label, but nobody has listened to it);
the subscription editor's error states; the rule editor's site, list and
address adders (only the app picker is drawn); the screen that lists every
network; and what the heart does while the always-on VPN is locked down by
Android.
