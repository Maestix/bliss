package app.bliss

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.VpnService
import android.os.Binder
import android.os.IBinder
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
        private const val CHANNEL = "tunnel"
        private const val NOTE = 1

        fun connect(context: Context) {
            context.startForegroundService(Intent(context, TunnelService::class.java).setAction(CONNECT))
        }
    }

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private var watcher: EngineClient? = null
    private var watching: Job? = null
    private var foreground = false
    private var lastState = "off"
    /** The ticket of the TUN this service handed over; see `Frame.ticket`. */
    private var ticket = 0L

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
            DISCONNECT -> Native.disconnect()
            // CONNECT, or null when Android's always-on VPN starts the service.
            else -> connect()
        }
        return START_NOT_STICKY
    }

    private fun connect() {
        enterForeground("Подключение…", "")
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
            // Inside the tunnel: lookups go through the server, not around it.
            .addDnsServer("1.1.1.1")
            .addDisallowedApplication(packageName)
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
        val name = frame.current?.let { frame.servers.getOrNull(it)?.name } ?: ""
        // The engine has let go of the TUN this service handed it: failed,
        // or switched off. A frame from before the hand-over has an older
        // ticket and says nothing about it.
        if (foreground && ticket > 0 && frame.ticket >= ticket && !frame.holding) {
            leaveForeground()
        } else when (frame.state) {
            "on" -> enterForeground("Подключено", name)
            "busy" -> if (foreground) enterForeground("Подключение…", name)
        }
        if (frame.state != lastState) {
            lastState = frame.state
            TileService.requestListeningState(this, ComponentName(this, Tile::class.java))
        }
    }

    private fun enterForeground(title: String, text: String) {
        val manager = getSystemService(NotificationManager::class.java)
        if (manager.getNotificationChannel(CHANNEL) == null) {
            // Silent: no sound, no vibration, low in the shade.
            manager.createNotificationChannel(
                NotificationChannel(CHANNEL, "Туннель", NotificationManager.IMPORTANCE_LOW).apply {
                    setShowBadge(false)
                }
            )
        }
        val off = PendingIntent.getService(
            this, 1, Intent(this, TunnelService::class.java).setAction(DISCONNECT),
            PendingIntent.FLAG_IMMUTABLE,
        )
        val note = Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_heart)
            .setColor(getColor(R.color.accent))
            .setContentTitle(if (text.isEmpty()) title else "$title · $text")
            .setContentIntent(openApp())
            .setOngoing(true)
            .addAction(Notification.Action.Builder(null, "Отключить", off).build())
            .build()
        startForeground(NOTE, note, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        foreground = true
    }

    private fun leaveForeground() {
        ticket = 0
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
        Native.disconnect()
        super.onRevoke()
    }

    override fun onDestroy() {
        watching?.cancel()
        watcher?.stop()
        scope.cancel()
        super.onDestroy()
    }
}
