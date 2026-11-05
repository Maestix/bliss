package app.bliss

import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.util.LruCache
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.graphics.drawable.toBitmap
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.File

/** An app a person can open: what a rule by app is about. */
data class AppInfo(val pkg: String, val label: String)

/**
 * The phone's apps, as the launcher shows them. Since Android 11 an app sees
 * only what it declares; the manifest's `<queries>` for launcher activities
 * shows every app a person can open, without QUERY_ALL_PACKAGES.
 */
object Apps {
    private val icons = LruCache<String, ImageBitmap>(160)
    private val labels = LruCache<String, String>(512)

    suspend fun load(context: Context): List<AppInfo> = withContext(Dispatchers.IO) {
        val pm = context.packageManager
        val launcher = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
        @Suppress("DEPRECATION") pm.queryIntentActivities(launcher, 0)
            .map { it.activityInfo.packageName }
            .distinct()
            .filter { it != context.packageName }
            .map { AppInfo(it, label(context, it)) }
            .sortedBy { it.label.lowercase() }
    }

    /** The app's name, or the package when it is not installed. */
    fun label(context: Context, pkg: String): String {
        labels.get(pkg)?.let { return it }
        val label = try {
            val pm = context.packageManager
            @Suppress("DEPRECATION") pm.getApplicationLabel(pm.getApplicationInfo(pkg, 0)).toString()
        } catch (_: PackageManager.NameNotFoundException) {
            pkg
        }
        labels.put(pkg, label)
        return label
    }

    fun icon(context: Context, pkg: String): ImageBitmap? {
        icons.get(pkg)?.let { return it }
        val bitmap = try {
            context.packageManager.getApplicationIcon(pkg).toBitmap(96, 96).asImageBitmap()
        } catch (_: Exception) {
            return null
        }
        icons.put(pkg, bitmap)
        return bitmap
    }
}

/** An app's icon, loaded off the main thread; the first letter until then. */
@Composable
fun AppIcon(pkg: String, label: String, size: Dp = 40.dp) {
    val context = LocalContext.current
    val icon by produceState<ImageBitmap?>(null, pkg) {
        value = withContext(Dispatchers.IO) { Apps.icon(context, pkg) }
    }
    val bitmap = icon
    if (bitmap != null) {
        Image(bitmap, contentDescription = null, modifier = Modifier.size(size))
    } else {
        Box(
            Modifier.size(size).clip(RoundedCornerShape(size / 3)).background(C.Accent.copy(alpha = 0.12f)),
            contentAlignment = Alignment.Center,
        ) { Text(label.take(1).uppercase(), style = T.body.copy(color = C.Accent, fontSize = 16.sp)) }
    }
}

/**
 * Apps left out of the VPN altogether («Полностью мимо VPN»): a list the
 * service reads each time it builds the interface. The screens write it and
 * ask the service to build again; the engine never needs it.
 */
object Bypass {
    private fun file(context: Context) = File(context.filesDir, "bypass.txt")

    /** Moves with every change, so a screen that shows the list reads it again. */
    private val changes = androidx.compose.runtime.mutableIntStateOf(0)
    val version: Int get() = changes.intValue

    fun read(context: Context): Set<String> = try {
        file(context).readLines().map { it.trim() }.filter { it.isNotEmpty() }.toSet()
    } catch (_: Exception) {
        emptySet()
    }

    fun set(context: Context, pkg: String, out: Boolean) {
        val now = read(context).toMutableSet()
        if (out) now.add(pkg) else now.remove(pkg)
        val target = file(context)
        val tmp = File(context.filesDir, "bypass.txt.new")
        tmp.writeText(now.sorted().joinToString("\n"))
        tmp.renameTo(target)
        changes.intValue++
        TunnelService.rebuild(context)
    }
}
