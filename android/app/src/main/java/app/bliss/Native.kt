package app.bliss

import android.content.Context
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
    external fun disconnect()

    fun ensure(context: Context): Boolean =
        start(context.filesDir.path, context.applicationInfo.nativeLibraryDir, socket(context))

    fun socket(context: Context): String = File(context.filesDir, "engine.sock").path
}
