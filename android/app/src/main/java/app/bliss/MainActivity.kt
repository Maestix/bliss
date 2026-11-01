package app.bliss

import android.Manifest
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.content.pm.PackageManager
import android.net.VpnService
import android.os.Build
import android.os.Bundle
import android.os.IBinder
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.delay

enum class Tab(val label: String) { Home("Главная"), Servers("Серверы"), Rules("Правила"), Settings("Настройки") }

/** Sheets over the tabs: one at a time. */
sealed interface Sheet {
    data object Server : Sheet
    data class Add(val text: String = "") : Sheet
    data class ServerMenu(val index: Int) : Sheet
    data class Group(val id: String) : Sheet
    data object Permission : Sheet
}

/** The engine's commands, as the screens call them: `c("select", "index" to 3)`. */
class Commands(private val send: (String, Array<out Pair<String, Any?>>) -> Unit) {
    operator fun invoke(name: String, vararg pairs: Pair<String, Any?>) = send(name, pairs)
}

class MainActivity : ComponentActivity() {
    private lateinit var client: EngineClient
    private var bound = false
    private val keepAlive = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName?, service: IBinder?) {}
        override fun onServiceDisconnected(name: ComponentName?) {}
    }
    private var shared by mutableStateOf<String?>(null)

    private val consent = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        if (VpnService.prepare(this) == null) {
            askNotifications()
            TunnelService.connect(this)
        }
    }
    private val notifications = registerForActivityResult(ActivityResultContracts.RequestPermission()) {}

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.dark(android.graphics.Color.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.dark(android.graphics.Color.TRANSPARENT),
        )
        super.onCreate(savedInstanceState)
        client = EngineClient(java.io.File(filesDir, "engine.sock").path, full = true)
        shared = linkFrom(intent)
        setContent {
            BlissTheme {
                val frame by client.frames.collectAsStateWithLifecycle()
                App(
                    frame = frame,
                    send = client::command,
                    connect = ::connect,
                    shared = shared,
                    consumeShared = { shared = null },
                )
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        linkFrom(intent)?.let { shared = it }
    }

    private fun linkFrom(intent: Intent?): String? = when (intent?.action) {
        Intent.ACTION_SEND -> intent.getStringExtra(Intent.EXTRA_TEXT)
        Intent.ACTION_VIEW -> intent.dataString
        else -> null
    }

    override fun onStart() {
        super.onStart()
        // Bound while the screens are up: it starts the tunnel's process and
        // the engine in it, and keeps them while someone looks.
        bound = bindService(
            Intent(this, TunnelService::class.java).setAction(TunnelService.BIND),
            keepAlive, Context.BIND_AUTO_CREATE,
        )
        client.start()
    }

    override fun onStop() {
        client.stop()
        if (bound) unbindService(keepAlive)
        bound = false
        super.onStop()
    }

    /** The heart: ask Android first if it has not been asked; priming is the app's. */
    private fun connect(primed: Boolean): Boolean {
        val ask = VpnService.prepare(this) ?: run {
            askNotifications()
            TunnelService.connect(this)
            return true
        }
        if (!primed) return false
        consent.launch(ask)
        return true
    }

    private fun askNotifications() {
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) notifications.launch(Manifest.permission.POST_NOTIFICATIONS)
    }
}

@Composable
fun App(
    frame: Frame,
    send: (String, Array<out Pair<String, Any?>>) -> Unit,
    connect: (Boolean) -> Boolean,
    shared: String?,
    consumeShared: () -> Unit,
) {
    val c = remember(send) { Commands(send) }
    var tab by remember { mutableStateOf(Tab.Home) }
    var traffic by remember { mutableStateOf(false) }
    var sheet by remember { mutableStateOf<Sheet?>(null) }

    LaunchedEffect(shared) {
        if (shared != null) {
            sheet = Sheet.Add(shared)
            consumeShared()
        }
    }

    val heart = {
        if (frame.state == "on" || frame.state == "busy") c("disconnect")
        else if (frame.servers.isEmpty()) sheet = Sheet.Add()
        else if (!connect(false)) sheet = Sheet.Permission
    }

    BackHandler(enabled = sheet != null || traffic || tab != Tab.Home) {
        when {
            sheet != null -> sheet = null
            traffic -> traffic = false
            else -> tab = Tab.Home
        }
    }

    Box(Modifier.fillMaxSize().background(C.Bg)) {
        if (traffic) {
            Traffic(frame, c = c, back = { traffic = false })
        } else {
            Column(Modifier.fillMaxSize()) {
                Box(Modifier.weight(1f)) {
                    AnimatedContent(tab, transitionSpec = { fadeIn() togetherWith fadeOut() }, label = "tab") { t ->
                        when (t) {
                            Tab.Home -> Home(frame, heart = heart, openServer = { sheet = Sheet.Server },
                                openTraffic = { traffic = true }, c = c, add = { sheet = Sheet.Add() })
                            Tab.Servers -> Servers(frame, c = c, add = { sheet = Sheet.Add() },
                                menu = { sheet = Sheet.ServerMenu(it) }, group = { sheet = Sheet.Group(it) })
                            Tab.Rules -> Rules(frame, c = c)
                            Tab.Settings -> Settings(frame, openTraffic = { traffic = true })
                        }
                    }
                }
                NavBar(tab) { tab = it }
            }
        }

        Snack(frame, restore = { c("restore") }, modifier = Modifier.align(Alignment.BottomCenter).padding(bottom = 96.dp))

        when (val s = sheet) {
            null -> {}
            Sheet.Server -> ServerSheet(frame, c = c, close = { sheet = null }, all = { sheet = null; tab = Tab.Servers })
            is Sheet.Add -> AddSheet(s.text, c = c, close = { sheet = null })
            is Sheet.ServerMenu -> ServerMenu(frame, s.index, c = c, close = { sheet = null })
            is Sheet.Group -> GroupMenu(frame, s.id, c = c, close = { sheet = null })
            Sheet.Permission -> PermissionSheet(close = { sheet = null }, go = { sheet = null; connect(true) })
        }
    }
}

@Composable
fun NavBar(tab: Tab, pick: (Tab) -> Unit) {
    Row(
        Modifier.fillMaxWidth().background(C.Surface.copy(alpha = 0.97f)).navigationBarsPadding()
            .height(72.dp).padding(horizontal = 4.dp, vertical = 8.dp),
    ) {
        for (t in Tab.entries) {
            val on = t == tab
            val icon: ImageVector = when (t) {
                Tab.Home -> if (on) Icons.HeartFilled else Icons.Heart
                Tab.Servers -> Icons.Globe
                Tab.Rules -> Icons.Split
                Tab.Settings -> Icons.Gear
            }
            Column(
                Modifier.weight(1f).clip(RoundedCornerShape(16.dp)).clickable { pick(t) },
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Box(
                    Modifier.size(58.dp, 32.dp).clip(RoundedCornerShape(16.dp))
                        .background(if (on) C.Accent.copy(alpha = 0.14f) else Color.Transparent),
                    contentAlignment = Alignment.Center,
                ) { Ico(icon, if (on) C.Accent else C.N400) }
                Text(t.label, style = T.tiny.copy(fontSize = 11.5.sp, color = if (on) C.Accent else C.N400))
            }
        }
    }
}

/** A line about what just happened, with «Вернуть» for a deletion; six seconds. */
@Composable
fun Snack(frame: Frame, restore: () -> Unit, modifier: Modifier = Modifier) {
    var shown by remember { mutableStateOf<Toast?>(null) }
    var seen by remember { mutableStateOf<Long>(-1) }
    LaunchedEffect(frame.toast?.id) {
        val t = frame.toast ?: return@LaunchedEffect
        if (seen == -1L) { seen = t.id; return@LaunchedEffect } // not one from before the screen opened
        if (t.id == seen) return@LaunchedEffect
        seen = t.id
        shown = t
        delay(6000)
        if (shown?.id == t.id) shown = null
    }
    val t = shown ?: return
    Row(
        modifier.padding(horizontal = 16.dp).fillMaxWidth().clip(RoundedCornerShape(18.dp))
            .background(C.Snack).padding(start = 18.dp, end = 6.dp, top = 6.dp, bottom = 6.dp).height(44.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(t.text, style = T.body.copy(fontSize = 14.sp, color = if (t.bad) C.BadText else C.Text), modifier = Modifier.weight(1f))
        if (t.undo && frame.canRestore) {
            Text("Вернуть", style = T.body.copy(color = C.Accent), modifier = Modifier.clip(RoundedCornerShape(20.dp))
                .clickable { restore(); shown = null }.padding(horizontal = 14.dp, vertical = 10.dp))
        }
    }
}
