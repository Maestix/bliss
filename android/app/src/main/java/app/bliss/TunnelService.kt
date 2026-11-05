package app.bliss

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.VpnService
import android.content.pm.PackageManager
import android.os.Binder
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.SystemClock
import android.view.View
import android.widget.RemoteViews
import android.service.quicksettings.TileService
import android.content.ComponentName
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.wifi.WifiInfo
import android.net.wifi.WifiManager
import android.os.Build
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch

/**
 * The VPN service, in the `:tunnel` process. It builds the TUN — address,
 * routes, DNS, and bliss's own app left out so its sockets go around — and
 * hands the descriptor to the engine, which owns it from then on. It watches
 * the engine's state over the same socket the screens use: when the tunnel
 * goes off or fails, the engine has already closed the descriptor, and the
 * service steps out of the foreground, so the phone is never left with a VPN
 * that leads nowhere.
 */
class TunnelService : VpnService() {
    companion object {
        const val CONNECT = "app.bliss.CONNECT"
        const val DISCONNECT = "app.bliss.DISCONNECT"
        const val BIND = "app.bliss.BIND"
        const val PAUSE = "app.bliss.PAUSE"
        const val RESUME = "app.bliss.RESUME"
        const val REBUILD = "app.bliss.REBUILD"
        /** Say the network again: the person just let bliss read Wi-Fi names. */
        const val NETWORK = "app.bliss.NETWORK"
        /** The phone's DNS server: inside the TUN's /30, answered by the
         *  engine itself, which is what lets rules see sites by name
         *  (`DNS_ADDRESS` in crates/android/src/engine.rs). */
        const val DNS = "172.19.0.2"
        private const val CHANNEL = "tunnel"
        private const val NOTE = 1
        private const val PAUSE_MS = 5 * 60 * 1000L

        fun connect(context: Context) {
            context.startForegroundService(Intent(context, TunnelService::class.java).setAction(CONNECT))
        }

        /** Build the interface again — an app left out or let back in — if it is up. */
        fun rebuild(context: Context) {
            context.startService(Intent(context, TunnelService::class.java).setAction(REBUILD))
        }

        fun reportNetwork(context: Context) {
            context.startService(Intent(context, TunnelService::class.java).setAction(NETWORK))
        }
    }

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private var watcher: EngineClient? = null
    private var watching: Job? = null
    private var foreground = false
    private var lastState = "off"
    /** The ticket of the TUN this service handed over; see `Frame.ticket`. */
    private var ticket = 0L
    /** While paused: when the tunnel comes back, on SystemClock.elapsedRealtime. */
    private var resumeAt: Long? = null
    private val handler = Handler(Looper.getMainLooper())
    private val resume = Runnable { connect() }
    /** What the notification shows now, so a frame that changes nothing posts nothing. */
    private var shown = ""
    /** Off because the network asked it (`Frame.wish` "off"): the service
     *  stays in the foreground, waiting, to see the next network. */
    private var waitingOn: String? = null
    /** The last network wish acted on, so each is acted on once. */
    private var lastWish = 0L
    /** The network as last said to the engine, so a repeat is not sent. */
    private var lastNetwork = ""
    private var networks: ConnectivityManager.NetworkCallback? = null

    override fun onCreate() {
        super.onCreate()
        Native.ensure(this)
        val client = EngineClient(Native.socket(this), full = false)
        client.start()
        watcher = client
        watching = scope.launch { client.frames.collect { onFrame(it) } }
        watchNetwork()
    }

    /**
     * The phone's default network, watched rather than polled, said to the
     * engine on every change: its kind, a Wi-Fi's name when bliss may read
     * it — Android gives an app the SSID only with location permission —
     * and whether it wants a sign-in. bliss's own app is left out of its
     * VPN, so this is the network under the tunnel, not the tunnel.
     */
    private fun watchNetwork() {
        val manager = getSystemService(ConnectivityManager::class.java) ?: return
        val callback = if (Build.VERSION.SDK_INT >= 31) {
            object : ConnectivityManager.NetworkCallback(FLAG_INCLUDE_LOCATION_INFO) {
                override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) = said(caps)
                override fun onLost(network: Network) = said(null)
            }
        } else {
            object : ConnectivityManager.NetworkCallback() {
                override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) = said(caps)
                override fun onLost(network: Network) = said(null)
            }
        }
        try {
            manager.registerDefaultNetworkCallback(callback, handler)
            networks = callback
        } catch (_: Exception) { }
    }

    private var lastCaps: NetworkCapabilities? = null

    private fun said(caps: NetworkCapabilities?) {
        lastCaps = caps
        val kind = when {
            caps == null -> null
            caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) -> "wifi"
            caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) -> "mobile"
            caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) -> "ethernet"
            else -> "other"
        }
        val ssid = if (kind == "wifi") wifiName(caps) else null
        val captive = caps?.hasCapability(NetworkCapabilities.NET_CAPABILITY_CAPTIVE_PORTAL) == true &&
            !caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
        val key = "$kind|$ssid|$captive"
        if (key == lastNetwork) return
        lastNetwork = key
        watcher?.command("network", "kind" to kind, "ssid" to ssid, "captive" to captive)
    }

    /** The Wi-Fi's name, or none when Android will not say it. */
    private fun wifiName(caps: NetworkCapabilities?): String? {
        val raw = if (Build.VERSION.SDK_INT >= 31) {
            (caps?.transportInfo as? WifiInfo)?.ssid
        } else {
            @Suppress("DEPRECATION")
            applicationContext.getSystemService(WifiManager::class.java)?.connectionInfo?.ssid
        }
        val name = raw?.removeSurrounding("\"")
        return name?.takeIf { it.isNotBlank() && it != WifiManager.UNKNOWN_SSID.removeSurrounding("\"") && it != "<unknown ssid>" }
    }

    override fun onBind(intent: Intent?): IBinder? =
        if (intent?.action == SERVICE_INTERFACE) super.onBind(intent) else Binder()

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            DISCONNECT -> {
                val wasResting = resumeAt != null || waitingOn != null
                stopPause()
                waitingOn = null
                Native.disconnect()
                if (wasResting) leaveForeground()
            }
            PAUSE -> pause()
            NETWORK -> { lastNetwork = ""; said(lastCaps) }
            REBUILD -> if (foreground && resumeAt == null && (lastState == "on" || lastState == "busy")) connect()
            // CONNECT, RESUME, or null when Android's always-on VPN starts the service.
            else -> connect()
        }
        return START_NOT_STICKY
    }

    /**
     * Five minutes without the tunnel, for an app or a page that will not
     * work through it, and then it comes back by itself — which a «switch
     * off and remember to switch on» never does. Not under Android's
     * «block connections without VPN», where it would mean no network.
     */
    private fun pause() {
        if (isLockdownEnabled || !foreground) return
        ticket = 0
        resumeAt = SystemClock.elapsedRealtime() + PAUSE_MS
        handler.removeCallbacks(resume)
        handler.postDelayed(resume, PAUSE_MS)
        Native.disconnect()
        enterForeground(Note.Paused(resumeAt!!))
    }

    private fun stopPause() {
        resumeAt = null
        handler.removeCallbacks(resume)
    }

    private fun connect() {
        stopPause()
        waitingOn = null
        enterForeground(Note.Busy(""))
        if (prepare(this) != null) {
            // No consent yet: only the screens can ask for it.
            leaveForeground()
            return
        }
        // establish() throws when consent was taken back in between (another
        // VPN switched on) or the system refuses: the tunnel's process must
        // not die of it.
        val tun = try { Builder()
            .setSession("bliss")
            .setMtu(1500)
            .addAddress("172.19.0.1", 30)
            .addRoute("0.0.0.0", 0)
            // No IPv6 address and no IPv6 route: Android then answers an
            // app's IPv6 with "unreachable" at once (allowFamily's default),
            // and it uses IPv4 — nothing leaks around the tunnel. Routed in
            // and refused by a rule, an IPv6 connection was first accepted by
            // the stack and then closed, which a browser takes as the site
            // closing on it (ERR_CONNECTION_CLOSED) and does not retry on IPv4.
            // bliss answers the lookups itself (names.rs in crates/tun), and
            // what it does not answer goes through the server.
            .addDnsServer(DNS)
            .addDisallowedApplication(packageName)
            .apply {
                // «Полностью мимо VPN»: apps that refuse to work while they see one.
                for (pkg in Bypass.read(this@TunnelService)) {
                    try { addDisallowedApplication(pkg) } catch (_: PackageManager.NameNotFoundException) { }
                }
            }
            .setConfigureIntent(openApp())
            .setMetered(false)
            .establish() } catch (_: Exception) { null }
        if (tun == null) {
            leaveForeground()
            return
        }
        ticket = Native.connect(tun.detachFd())
        if (ticket == 0L) leaveForeground()
    }

    private fun onFrame(frame: Frame) {
        if (!frame.connected) return
        if (frame.state != lastState) {
            lastState = frame.state
            TileService.requestListeningState(this, ComponentName(this, Tile::class.java))
        }
        // What the network asks — connect, or stay off — once per network.
        if (frame.wishId > lastWish) {
            lastWish = frame.wishId
            val name = frame.network?.name ?: ""
            when (frame.wish) {
                "on" -> if (frame.state != "on" && frame.state != "busy" && resumeAt == null && prepare(this) == null) {
                    connect()
                    return
                }
                "back" -> if (waitingOn != null && prepare(this) == null) {
                    connect()
                    return
                }
                "off" -> if (!isLockdownEnabled && (frame.state == "on" || frame.state == "busy" || waitingOn != null)) {
                    ticket = 0
                    waitingOn = name
                    Native.disconnect()
                    enterForeground(Note.Waiting(name))
                    return
                }
            }
        }
        // Paused: the engine is off on purpose, and the notification counts down.
        if (resumeAt != null) return
        // Waiting out a network bliss was told to stay off on.
        if (waitingOn != null) return
        val server = frame.current?.let { frame.servers.getOrNull(it) }
        val name = server?.name ?: ""
        // The engine has let go of the TUN this service handed it: failed,
        // or switched off. A frame from before the hand-over has an older
        // ticket and says nothing about it.
        if (foreground && ticket > 0 && frame.ticket >= ticket && !frame.holding) {
            leaveForeground()
        } else when (frame.state) {
            "on" -> enterForeground(Note.On(name, frame.since ?: System.currentTimeMillis(), frame.numbers.latency))
            "busy" -> if (foreground) enterForeground(Note.Busy(name))
        }
    }

    /** What the notification is about. */
    private sealed interface Note {
        data class Busy(val server: String) : Note
        data class On(val server: String, val since: Long, val latency: Int?) : Note
        data class Paused(val until: Long) : Note
        data class Waiting(val network: String) : Note
    }

    private fun enterForeground(note: Note) {
        val manager = getSystemService(NotificationManager::class.java)
        if (manager.getNotificationChannel(CHANNEL) == null) {
            // Silent: no sound, no vibration, low in the shade.
            manager.createNotificationChannel(
                NotificationChannel(CHANNEL, "Туннель", NotificationManager.IMPORTANCE_LOW).apply {
                    setShowBadge(false)
                }
            )
        }
        val key = note.toString()
        if (foreground && key == shown) return
        shown = key
        startForeground(NOTE, build(note), ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        foreground = true
    }

    /**
     * The notification is bliss's own card (res/layout/note_*.xml): the
     * heart, the state in the accent, the server, a clock the system ticks
     * by itself — nothing here wakes every second — and pills for what a
     * person reaches for from anywhere: a pause, the servers, off.
     */
    private fun build(note: Note): Notification {
        val small = RemoteViews(packageName, R.layout.note_small)
        val big = RemoteViews(packageName, R.layout.note_big)
        val pills = listOf(R.id.first, R.id.second, R.id.third)
        fun pill(at: Int, text: String, intent: PendingIntent) {
            big.setViewVisibility(pills[at], View.VISIBLE)
            big.setTextViewText(pills[at], text)
            big.setOnClickPendingIntent(pills[at], intent)
        }
        for (id in pills) big.setViewVisibility(id, View.GONE)
        fun clock(base: Long, down: Boolean) {
            for (views in listOf(small, big)) {
                views.setViewVisibility(R.id.clock, View.VISIBLE)
                views.setChronometer(R.id.clock, base, null, true)
                views.setChronometerCountDown(R.id.clock, down)
            }
        }
        val title: String
        when (note) {
            is Note.Busy -> {
                title = "Подключение…"
                for (views in listOf(small, big)) {
                    views.setTextViewText(R.id.title, title)
                    views.setTextViewText(R.id.detail, note.server.ifEmpty { "движок, сервер, проверка" })
                    views.setViewVisibility(R.id.clock, View.GONE)
                }
                big.setTextViewText(R.id.clock_word, "")
                big.setTextViewText(R.id.latency, "")
                pill(0, "Выкл.", service(DISCONNECT, 1))
            }
            is Note.On -> {
                title = "Подключено"
                small.setTextViewText(R.id.title, title)
                small.setTextViewText(R.id.detail, note.server + (note.latency?.let { " · $it мс" } ?: ""))
                big.setTextViewText(R.id.title, title)
                big.setTextViewText(R.id.detail, note.server)
                big.setTextViewText(R.id.clock_word, "в сети")
                big.setTextViewText(R.id.latency, note.latency?.let { " · $it мс" } ?: "")
                clock(SystemClock.elapsedRealtime() - (System.currentTimeMillis() - note.since), false)
                var at = 0
                if (!isLockdownEnabled) pill(at++, "Пауза", service(PAUSE, 2))
                pill(at++, "Сервер", PendingIntent.getActivity(
                    this, 3, Intent(this, MainActivity::class.java).setAction(OPEN_SERVERS),
                    PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
                ))
                pill(at, "Выкл.", service(DISCONNECT, 1))
            }
            is Note.Paused -> {
                title = "Пауза"
                // The bunny asleep in its cup: the tunnel is resting, not broken.
                big.setImageViewResource(R.id.picture, R.drawable.bunny)
                for (views in listOf(small, big)) views.setTextViewText(R.id.title, title)
                small.setTextViewText(R.id.detail, "вернусь сам")
                big.setTextViewText(R.id.detail, "Туннель отдыхает и вернётся сам")
                big.setTextViewText(R.id.clock_word, "через")
                big.setTextViewText(R.id.latency, "")
                clock(note.until, true)
                pill(0, "Продолжить", service(RESUME, 4))
                pill(1, "Выкл.", service(DISCONNECT, 1))
            }
            is Note.Waiting -> {
                title = "Ждёт другой сети"
                big.setImageViewResource(R.id.picture, R.drawable.bunny)
                for (views in listOf(small, big)) {
                    views.setTextViewText(R.id.title, title)
                    views.setViewVisibility(R.id.clock, View.GONE)
                }
                small.setTextViewText(R.id.detail, "в «${note.network}» — без туннеля")
                big.setTextViewText(R.id.detail, "В «${note.network}» не подключаюсь, вернусь в другой сети")
                big.setTextViewText(R.id.clock_word, "")
                big.setTextViewText(R.id.latency, "")
                pill(0, "Подключить", service(CONNECT, 5))
                pill(1, "Выкл.", service(DISCONNECT, 1))
            }
        }
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_heart)
            // The whole card in the program's surface colour, which the
            // scene's edges fade into: a foreground service may colour its
            // notification, and without it the card stood as a box inside
            // the system's (the user's screenshot of 0.2.0).
            .setColor(getColor(R.color.surface))
            .setColorized(true)
            .setContentTitle(title)
            .setStyle(Notification.DecoratedCustomViewStyle())
            .setCustomContentView(small)
            .setCustomBigContentView(big)
            .setContentIntent(openApp())
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setShowWhen(false)
            .build()
    }

    private fun service(action: String, code: Int): PendingIntent = PendingIntent.getService(
        this, code, Intent(this, TunnelService::class.java).setAction(action), PendingIntent.FLAG_IMMUTABLE,
    )

    private fun leaveForeground() {
        ticket = 0
        shown = ""
        if (!foreground) return
        stopForeground(STOP_FOREGROUND_REMOVE)
        foreground = false
        stopSelf()
    }

    private fun openApp(): PendingIntent = PendingIntent.getActivity(
        this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
    )

    override fun onRevoke() {
        // Another VPN took over, or the person revoked consent.
        stopPause()
        Native.disconnect()
        super.onRevoke()
    }

    override fun onDestroy() {
        handler.removeCallbacks(resume)
        networks?.let { try { getSystemService(ConnectivityManager::class.java)?.unregisterNetworkCallback(it) } catch (_: Exception) { } }
        watching?.cancel()
        watcher?.stop()
        scope.cancel()
        super.onDestroy()
    }
}
