package app.bliss

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** Connections and the journal: a screen over the tabs, opened from Главная. */
@Composable
fun Traffic(frame: Frame, c: Commands, back: () -> Unit) {
    var tab by remember { mutableStateOf("conns") }
    // The table and the log cross the socket only while this screen asks.
    DisposableEffect(tab) {
        c("table", "open" to (tab == "conns"))
        c("log", "open" to (tab == "log"))
        onDispose { c("table", "open" to false); c("log", "open" to false) }
    }
    Column(Modifier.fillMaxSize().statusBarsPadding().navigationBarsPadding().padding(horizontal = 12.dp)) {
        Row(Modifier.height(56.dp), verticalAlignment = Alignment.CenterVertically) {
            IconBtn(Icons.Back, C.N300, "Назад", back)
            Column {
                Text("Трафик", style = T.title.copy(fontSize = 22.sp))
                Text("${plural(frame.numbers.active.toLong(), "соединение", "соединения", "соединений")} сейчас · ${frame.numbers.opened} за сессию",
                    style = T.tiny.copy(fontSize = 12.5.sp))
            }
        }
        Column(Modifier.padding(horizontal = 4.dp, vertical = 8.dp).fillMaxWidth().card().padding(16.dp)) {
            Row(verticalAlignment = Alignment.Bottom) {
                Text("↓ ${rate(frame.numbers.downRate)}", style = T.body.copy(fontWeight = FontWeight.Bold, fontSize = 20.sp, color = C.Accent300))
                Spacer(Modifier.width(14.dp))
                Text("↑ ${rate(frame.numbers.upRate)}", style = T.body.copy(color = C.N400))
            }
            RateChart(frame.history, active = true, modifier = Modifier.padding(vertical = 10.dp).fillMaxWidth().height(56.dp))
            Text("за сессию ↓ ${bytes(frame.numbers.downTotal)} · ↑ ${bytes(frame.numbers.upTotal)}" +
                (frame.numbers.latency?.let { " · задержка $it мс" } ?: ""), style = T.tiny.copy(fontSize = 12.5.sp))
        }
        Row(
            Modifier.padding(horizontal = 4.dp, vertical = 4.dp).fillMaxWidth().clip(RoundedCornerShape(24.dp))
                .background(C.Tint).border(1.dp, Color(0x14EFE8F5), RoundedCornerShape(24.dp)).padding(4.dp),
        ) {
            for ((key, label) in listOf("conns" to "Соединения", "log" to "Журнал")) {
                val on = tab == key
                Box(
                    Modifier.weight(1f).height(40.dp).clip(RoundedCornerShape(20.dp))
                        .background(if (on) C.Accent.copy(alpha = 0.13f) else Color.Transparent).clickable { tab = key },
                    contentAlignment = Alignment.Center,
                ) { Text(label, style = T.body.copy(fontSize = 14.sp, fontWeight = FontWeight.Bold, color = if (on) C.Accent else C.N500)) }
            }
        }
        if (tab == "conns") {
            val rows = frame.table.orEmpty()
            LazyColumn(Modifier.fillMaxSize()) {
                if (rows.isEmpty()) item {
                    Text("Пока пусто — соединения появятся, когда туннель понесёт трафик.", style = T.small,
                        modifier = Modifier.padding(16.dp))
                }
                items(rows) { r ->
                    Row(
                        Modifier.fillMaxWidth().heightIn(min = 58.dp).padding(horizontal = 12.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Column(Modifier.weight(1f).padding(vertical = 6.dp)) {
                            Text(r.target, style = T.body.copy(color = if (r.closed) C.N500 else C.Text), maxLines = 1)
                            Text("↓ ${bytes(r.down)} · ↑ ${bytes(r.up)} · ${r.rule}", style = T.tiny.copy(fontSize = 12.5.sp), maxLines = 1)
                        }
                        Pill(r.route, r.closed)
                    }
                }
            }
        } else {
            val lines = frame.log.orEmpty()
            LazyColumn(Modifier.fillMaxSize().padding(top = 6.dp).clip(RoundedCornerShape(16.dp)).background(C.Sunk).padding(8.dp)) {
                items(lines.asReversed()) { line ->
                    val color = when {
                        " ERROR " in line -> C.BlockText
                        " WARN " in line -> Color(0xFFD9B16A)
                        else -> C.N300
                    }
                    Text(line, style = T.body.copy(fontFamily = FontFamily.Monospace, fontWeight = FontWeight.Normal,
                        fontSize = 12.sp, color = color), modifier = Modifier.padding(vertical = 3.dp))
                }
            }
        }
    }
}

