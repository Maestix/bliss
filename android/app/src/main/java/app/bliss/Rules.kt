package app.bliss

import androidx.compose.foundation.background
import androidx.compose.foundation.border
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
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * The routing screen: presets, what will not work, the rules with their
 * switches, and where everything else goes. Rule changes apply to the next
 * connection at once — no reconnect (AGENTS.md §5).
 */
@Composable
fun Rules(frame: Frame, c: Commands) {
    LazyColumn(Modifier.fillMaxSize().statusBarsPadding().padding(horizontal = 12.dp)) {
        item {
            Column(Modifier.padding(start = 4.dp, top = 8.dp, bottom = 12.dp)) {
                Text("Правила", style = T.title)
                val on = frame.rules.count { it.enabled }
                Text("${plural(frame.rules.size.toLong(), "правило", "правила", "правил")}, включено $on · остальное ${actionWord(frame.fallback)}",
                    style = T.small)
            }
        }
        if (frame.presets.isNotEmpty()) item {
            Row(Modifier.padding(bottom = 10.dp).horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                for (p in frame.presets) {
                    val on = p == frame.preset
                    Chip(p, on, dot = on && frame.drifted) { c("preset", "name" to p) }
                }
                if (frame.drifted && frame.preset != null) Chip("Сохранить", false, onClick = { c("save_preset") })
            }
        }
        for (w in frame.warnings.take(3)) item {
            Text(w, style = T.body.copy(fontSize = 13.sp, color = C.Violet300), modifier = Modifier.padding(bottom = 8.dp)
                .fillMaxWidth().clip(RoundedCornerShape(16.dp)).background(Color(0x12CBBAF4))
                .border(1.dp, Color(0x2ECBBAF4), RoundedCornerShape(16.dp)).padding(12.dp))
        }
        itemsIndexed(frame.rules) { i, r ->
            Row(
                Modifier.padding(bottom = 4.dp).fillMaxWidth().heightIn(min = 58.dp).clip(RoundedCornerShape(16.dp))
                    .background(Color(0x08EFE8F5)).padding(horizontal = 12.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Toggle(r.enabled) { c("rule", "index" to i, "on" to it) }
                Column(Modifier.weight(1f).padding(horizontal = 12.dp, vertical = 8.dp).alpha(if (r.enabled) 1f else 0.42f)) {
                    Row {
                        Text("${i + 1}", style = T.body.copy(color = C.N500))
                        Spacer(Modifier.width(6.dp))
                        Text(r.name, style = T.body, maxLines = 1)
                    }
                    Text(r.conditions, style = T.tiny.copy(fontSize = 12.5.sp), maxLines = 1)
                }
                Box(Modifier.alpha(if (r.enabled) 1f else 0.42f)) { Pill(r.action) }
            }
        }
        item {
            Row(Modifier.padding(top = 8.dp, start = 12.dp), verticalAlignment = Alignment.CenterVertically) {
                Text("Всё остальное", style = T.body.copy(fontSize = 14.sp), modifier = Modifier.weight(1f))
                for (a in listOf("proxy", "direct")) {
                    Box(Modifier.padding(start = 8.dp)) {
                        Chip(actionWord(a), frame.fallback == a, color = actionColor(a)) { c("fallback", "action" to a) }
                    }
                }
            }
        }
        item {
            Text("Редактор правил, правила для приложений и «мимо VPN» — в следующей сборке. " +
                "Пока здесь то, что пришло с профилем, и стартовое правило для локальной сети.",
                style = T.tiny.copy(fontSize = 12.5.sp), modifier = Modifier.padding(start = 12.dp, top = 18.dp, end = 12.dp))
        }
        item { Spacer(Modifier.height(24.dp)) }
    }
}
