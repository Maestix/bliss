package app.bliss

import android.content.Context
import android.net.ConnectivityManager
import android.os.Process
import android.system.OsConstants
import android.util.LruCache
import java.net.InetAddress
import java.net.InetSocketAddress
import java.io.File

/**
 * The engine's three entry points (crates/android/src/android.rs). Loaded only
 * in the `:tunnel` process: the screens talk to the engine over its socket.
 */
object Native {
    init {
        System.loadLibrary("bliss")
    }

    external fun start(files: String, libs: String, socket: String): Boolean
    /** Answers the ticket the engine's frames name this TUN by, or 0. */
    external fun connect(fd: Int): Long
    /** `byHand`: the person switched it off (the tile, the notification's
     *  «Выкл.»), and no network's rule switches it back on until they do. */
    external fun disconnect(byHand: Boolean)

    fun ensure(context: Context): Boolean {
        app = context.applicationContext
        return start(context.filesDir.path, context.applicationInfo.nativeLibraryDir, socket(context))
    }

    @Volatile private var app: Context? = null
    private val packages = LruCache<Int, String>(256)

    /**
     * Called by the engine for each connection through the TUN: the package
     * that opened it, for `app:` rules. Android answers this only for the
     * active VPN's own app, which this process is (API 29).
     */
    @JvmStatic
    fun ownerOf(tcp: Boolean, localIp: String, localPort: Int, remoteIp: String, remotePort: Int): String? {
        val context = app ?: return null
        return try {
            val uid = context.getSystemService(ConnectivityManager::class.java).getConnectionOwnerUid(
                if (tcp) OsConstants.IPPROTO_TCP else OsConstants.IPPROTO_UDP,
                InetSocketAddress(InetAddress.getByName(localIp), localPort),
                InetSocketAddress(InetAddress.getByName(remoteIp), remotePort),
            )
            if (uid == Process.INVALID_UID) return null
            packages.get(uid) ?: (context.packageManager.getPackagesForUid(uid)?.firstOrNull()
                ?: context.packageManager.getNameForUid(uid))?.also { packages.put(uid, it) }
        } catch (_: Exception) {
            null
        }
    }

    fun socket(context: Context): String = File(context.filesDir, "engine.sock").path
}
