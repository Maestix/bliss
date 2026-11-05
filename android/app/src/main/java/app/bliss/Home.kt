package app.bliss

import androidx.compose.animation.Crossfade
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay

@Composable
fun Home(frame: Frame, heart: () -> Unit, openServer: () -> Unit, openTraffic: () -> Unit, c: Commands, add: () -> Unit) {
    Box(Modifier.fillMaxSize()) {
        // The sky: the desktop's night scene, full width, faded into the
        // ground at both ends so it has no edge (docs/android-design.md).
        Image(
            painterResource(R.drawable.scene), contentDescription = null,
            contentScale = ContentScale.Crop, alignment = BiasAlignment71,
            modifier = Modifier.fillMaxWidth().height(520.dp).offset(y = 70.dp).drawWithContent {
                drawContent()
                drawRect(Brush.verticalGradient(
                    0f to C.Bg, 0.16f to Color.Transparent, 0.52f to Color.Transparent, 0.97f to C.Bg,
                ))
            },
        )
        Column(Modifier.fillMaxSize().statusBarsPadding().padding(horizontal = 16.dp).padding(top = 8.dp, bottom = 14.dp)) {
            Row(Modifier.height(44.dp), verticalAlignment = Alignment.CenterVertically) {
                Image(painterResource(R.drawable.mark), null, Modifier.size(30.dp))
                Spacer(Modifier.size(10.dp))
                Text("bliss", style = T.body.copy(fontWeight = FontWeight.Bold, fontSize = 22.sp))
            }
            TrafficCard(frame, openTraffic)
            Spacer(Modifier.weight(1f))

            Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally) {
                Ring(frame.state, 156.dp, heart)
                Spacer(Modifier.height(16.dp))
                // The word keeps its place: a box of one height, the whole
                // width, and only a crossfade between words. It slid in from
                // below and resized with each word, which read as a jump.
                Crossfade(frame.state, animationSpec = tween(220), label = "word",
                    modifier = Modifier.fillMaxWidth().height(36.dp)) { state ->
                    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                        Text(
                            when (state) { "on" -> "Подключено"; "busy" -> "Подключение"; "failed" -> "Ошибка"; else -> "Отключено" },
                            style = T.state.copy(color = when (state) {
                                "on" -> C.Accent; "busy" -> C.Violet; "failed" -> C.BlockText; else -> C.N500
                            }),
                        )
                    }
                }
                Box(Modifier.heightIn(min = 26.dp).padding(top = 4.dp), contentAlignment = Alignment.Center) {
                    when (frame.state) {
                        "busy" -> Steps(frame.step)
                        "on" -> Uptime(frame.since)
                        "failed" -> Text(frame.error ?: "", style = T.body.copy(fontSize = 14.sp, color = C.N400))
                        else -> Text(if (frame.servers.isEmpty()) "сначала добавьте сервер" else "нажмите на сердечко",
                            style = T.body.copy(fontSize = 14.sp, color = C.N400))
                    }
                }
                Box(Modifier.heightIn(min = 44.dp).padding(top = 2.dp), contentAlignment = Alignment.Center) {
                    val s = frame.suggest
                    when {
                        frame.state == "failed" && s != null ->
                            Btn("Подключить ${s.name} · ${s.ping} мс") { c("select", "index" to s.index); heart() }
                        frame.servers.isEmpty() -> Btn("Добавить сервер", icon = Icons.Plus, onClick = add)
                    }
                }
            }

            if (frame.presets.isNotEmpty()) {
                Row(
                    Modifier.padding(top = 16.dp).horizontalScroll(rememberScrollState()),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    for (p in frame.presets) {
                        val on = p == frame.preset
                        Chip(p, on, dot = on && frame.drifted) { c("preset", "name" to p) }
                    }
                }
            }

            Spacer(Modifier.weight(0.35f))
            ServerCard(frame, openServer)
        }
    }
}

private val BiasAlignment71 = androidx.compose.ui.BiasAlignment(0.42f, -1f)

/**
 * The top of the screen: what the tunnel is carrying, over the sky. The
 * rates, the chart of the last seconds, the delay and the session's totals;
 * a tap opens Трафик. Off, it says so quietly rather than showing zeros.
 */
@Composable
private fun TrafficCard(frame: Frame, open: () -> Unit) {
    val on = frame.state == "on"
    Column(
        Modifier.padding(top = 10.dp).fillMaxWidth().clip(S.card)
            .background(C.Surface.copy(alpha = 0.72f))
            .border(1.dp, Color(0x14EFE8F5), S.card)
            .clickable(onClick = open).padding(horizontal = 16.dp, vertical = 14.dp),
    ) {
        Row(verticalAlignment = Alignment.Bottom) {
            Column(Modifier.weight(1f)) {
                Text(if (on) "↓ ${rate(frame.numbers.downRate)}" else "Туннель выключен",
                    style = T.body.copy(fontWeight = FontWeight.Bold, fontSize = 20.sp, color = if (on) C.Accent300 else C.N400))
                Text(if (on) "↑ ${rate(frame.numbers.upRate)} · ${plural(frame.numbers.active.toLong(), "соединение", "соединения", "соединений")}"
                    else "скорость и соединения появятся здесь", style = T.small)
            }
            Column(horizontalAlignment = Alignment.End) {
                Text("задержка", style = T.tiny)
                Text(frame.numbers.latency?.let { "$it мс" } ?: "—", style = T.body)
            }
        }
        RateChart(frame.history, active = on, modifier = Modifier.padding(top = 10.dp, bottom = 8.dp).fillMaxWidth().height(44.dp))
        Row {
            Text("за сессию ↓ ${bytes(frame.numbers.downTotal)} · ↑ ${bytes(frame.numbers.upTotal)}", style = T.tiny, modifier = Modifier.weight(1f))
            Ico(Icons.Right, C.N500, 16.dp)
        }
    }
}

@Composable
private fun ServerCard(frame: Frame, open: () -> Unit) {
    val server = frame.current?.let { frame.servers.getOrNull(it) }
    Row(
        Modifier.padding(top = 12.dp).fillMaxWidth().height(72.dp).card().clickable(onClick = open)
            .padding(start = 14.dp, end = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(
            Modifier.size(44.dp).clip(RoundedCornerShape(14.dp)).background(C.Accent.copy(alpha = 0.10f)),
            contentAlignment = Alignment.Center,
        ) { Ico(Icons.Globe, C.Accent) }
        Column(Modifier.weight(1f).padding(horizontal = 14.dp)) {
            Text(server?.name?.let { if (frame.auto) "Автовыбор · $it" else it } ?: "Сервер не выбран", style = T.card, maxLines = 1)
            if (server != null) Row {
                if (checkText(server).isNotEmpty()) Text("доступ ${checkText(server)}", style = T.small.copy(color = checkColor(server)))
                else Text(pingText(server), style = T.small.copy(color = pingColor(server)))
                Text(" · ${server.detail}", style = T.small, maxLines = 1)
            } else Text("нажмите, чтобы выбрать или добавить", style = T.small)
        }
        Ico(Icons.Up, C.N500)
    }
}

/** «движок · сервер · проверка», each lit as it is done. */
@Composable
private fun Steps(step: Int) {
    Row(horizontalArrangement = Arrangement.spacedBy(14.dp), verticalAlignment = Alignment.CenterVertically) {
        listOf("движок", "сервер", "проверка").forEachIndexed { i, word ->
            val done = i < step
            val now = i == step
            Row(verticalAlignment = Alignment.CenterVertically) {
                Box(Modifier.size(7.dp).clip(CircleShape).background(
                    if (done || now) C.Violet else C.N700))
                Spacer(Modifier.size(6.dp))
                Text(word, style = T.small.copy(color = when { done -> C.Violet300; now -> C.Text; else -> C.N500 }))
            }
        }
    }
}

/** The clock ticks here, from when the tunnel came up: the engine sends no ticks. */
@Composable
private fun Uptime(since: Long?) {
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(since) {
        while (true) { now = System.currentTimeMillis(); delay(1000) }
    }
    val s = ((now - (since ?: now)) / 1000).coerceAtLeast(0)
    val text = "%02d:%02d:%02d".format(s / 3600, s / 60 % 60, s % 60)
    Row {
        Text("В сети ", style = T.body.copy(fontSize = 14.sp, color = C.N400))
        // Comfortaa has no tabular figures: each digit in a box as wide as the widest.
        for (ch in text) {
            Box(Modifier.size(width = if (ch.isDigit()) 9.5.dp else 5.dp, height = 20.dp), contentAlignment = Alignment.Center) {
                Text(ch.toString(), style = T.body.copy(fontSize = 14.sp, color = C.N400))
            }
        }
    }
}
