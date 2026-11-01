package app.bliss

import android.content.Intent
import android.net.VpnService
import android.service.quicksettings.Tile as QsTile
import android.service.quicksettings.TileService
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch

/** The quick settings tile: tap connects or disconnects; long press opens bliss. */
class Tile : TileService() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private var client: EngineClient? = null
    private var job: Job? = null
    private var state = "off"

    override fun onStartListening() {
        Native.ensure(this)
        val c = EngineClient(Native.socket(this), full = false)
        c.start()
        client = c
        job = scope.launch {
            c.frames.collect { frame ->
                if (!frame.connected) return@collect
                state = frame.state
                val tile = qsTile ?: return@collect
                tile.state = when (state) {
                    "on", "busy" -> QsTile.STATE_ACTIVE
                    else -> QsTile.STATE_INACTIVE
                }
                tile.subtitle = when (state) {
                    "on" -> frame.current?.let { frame.servers.getOrNull(it)?.name } ?: "Подключено"
                    "busy" -> "Подключение…"
                    "failed" -> "Ошибка"
                    else -> "Отключено"
                }
                tile.updateTile()
            }
        }
    }

    override fun onStopListening() {
        job?.cancel()
        client?.stop()
        client = null
    }

    override fun onClick() {
        if (state == "on" || state == "busy") {
            Native.disconnect()
            return
        }
        if (VpnService.prepare(this) != null) {
            // Consent is asked from the screens only.
            val open = Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            if (android.os.Build.VERSION.SDK_INT >= 34) {
                startActivityAndCollapse(
                    android.app.PendingIntent.getActivity(this, 0, open, android.app.PendingIntent.FLAG_IMMUTABLE)
                )
            } else {
                @Suppress("DEPRECATION")
                startActivityAndCollapse(open)
            }
            return
        }
        TunnelService.connect(this)
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }
}
