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

    override fun onCreate() {
        super.onCreate()
        Native.ensure(this)
        val client = EngineClient(Native.socket(this), full = false)
        client.start()
        watcher = client
        watching = scope.launch { client.frames.collect { onFrame(it) } }
    }

    override fun onBind(intent: Intent?): IBinder? =
        if (intent?.action == SERVICE_INTERFACE) super.onBind(intent) else Binder()

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            DISCONNECT -> {
                val wasPaused = resumeAt != null
                stopPause()
                Native.disconnect()
                if (wasPaused) leaveForeground()
            }
            PAUSE -> pause()
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
        enterForeground(Note.Busy(""))
        if (prepare(this) != null) {
            // No consent yet: only the screens can ask for it.
            leaveForeground()
            return
        }
        val tun = Builder()
            .setSession("bliss")
            .setMtu(1500)
            .addAddress("172.19.0.1", 30)
            .addRoute("0.0.0.0", 0)
            .addAddress("fdfe:dcba:9876::1", 126)
            .addRoute("::", 0)
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
            .establish()
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
        // Paused: the engine is off on purpose, and the notification counts down.
        if (resumeAt != null) return
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
                pill(0, "Отключить", service(DISCONNECT, 1))
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
                pill(at, "Отключить", service(DISCONNECT, 1))
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
                pill(1, "Отключить", service(DISCONNECT, 1))
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
        watching?.cancel()
        watcher?.stop()
        scope.cancel()
        super.onDestroy()
    }
}
