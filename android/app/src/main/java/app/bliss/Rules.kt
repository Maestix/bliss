package app.bliss

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * Правила: apps first (docs/android-design.md). Two views of one list: an
 * app's choice is an `app:` rule in «Список правил», editable there like any
 * other. Rule changes apply to the next connection at once.
 */
@Composable
fun Rules(frame: Frame, c: Commands, open: (Sheet) -> Unit) {
    var tab by rememberSaveable { mutableStateOf("apps") }
    Box(Modifier.fillMaxSize().statusBarsPadding()) {
        Column(Modifier.fillMaxSize()) {
            Column(Modifier.padding(start = 16.dp, top = 8.dp, end = 16.dp)) {
                Text("Правила", style = T.title)
                val on = frame.rules.count { it.enabled }
                Text("${plural(frame.rules.size.toLong(), "правило", "правила", "правил")}, включено $on · остальное ${actionWord(frame.fallback)}",
                    style = T.small)
            }
            Segments(listOf("apps" to "Приложения", "list" to "Список правил"), tab, Modifier.padding(horizontal = 12.dp, vertical = 10.dp)) { tab = it }
            if (tab == "apps") AppList(frame, open) else RuleList(frame, c, open)
        }
        if (tab == "list") {
            Row(
                Modifier.align(Alignment.BottomEnd).padding(16.dp).height(56.dp).clip(RoundedCornerShape(28.dp))
                    .background(Color(0xFF2A1F37)).border(1.dp, C.Accent.copy(alpha = 0.5f), RoundedCornerShape(28.dp))
                    .clickable { open(Sheet.RuleEdit(null)) }.padding(start = 18.dp, end = 22.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Ico(Icons.Plus, C.Accent)
                Spacer(Modifier.width(10.dp))
                Text("Правило", style = T.body.copy(fontWeight = FontWeight.Bold, color = C.Accent))
            }
        }
    }
}

/** Two or three choices side by side, one lit. */
@Composable
fun Segments(options: List<Pair<String, String>>, on: String, modifier: Modifier = Modifier, pick: (String) -> Unit) {
    Row(
        modifier.fillMaxWidth().clip(RoundedCornerShape(24.dp)).background(C.Tint)
            .border(1.dp, Color(0x14EFE8F5), RoundedCornerShape(24.dp)).padding(4.dp),
    ) {
        for ((key, label) in options) {
            val lit = key == on
            Box(
                Modifier.weight(1f).height(40.dp).clip(RoundedCornerShape(20.dp))
                    .background(if (lit) C.Accent.copy(alpha = 0.13f) else Color.Transparent).clickable { pick(key) },
                contentAlignment = Alignment.Center,
            ) { Text(label, style = T.body.copy(fontSize = 14.sp, fontWeight = FontWeight.Bold, color = if (lit) C.Accent else C.N500)) }
        }
    }
}

// --- apps --------------------------------------------------------------------

@Composable
private fun AppList(frame: Frame, open: (Sheet) -> Unit) {
    val context = LocalContext.current
    val apps by produceState<List<AppInfo>?>(null) { value = Apps.load(context) }
    var query by rememberSaveable { mutableStateOf("") }
    val bypass = remember(frame.rules) { Bypass.read(context) }
    val ruled = frame.rules.mapNotNull { r -> r.app?.let { it.lowercase() to r } }.toMap()
    val all = apps
    LazyColumn(Modifier.fillMaxSize().padding(horizontal = 12.dp)) {
        item { Search(query, "Найти приложение") { query = it } }
        if (all == null) item { Text("Собираю список приложений…", style = T.small, modifier = Modifier.padding(16.dp)) }
        else {
            val found = all.filter { query.isBlank() || it.label.contains(query, true) || it.pkg.contains(query, true) }
            // Apps with a way of their own first: they are what a person comes back to.
            val (own, rest) = found.partition { ruled.containsKey(it.pkg.lowercase()) || it.pkg in bypass }
            if (own.isNotEmpty()) item { Kicker("Со своим правилом", Modifier.padding(start = 12.dp, top = 8.dp, bottom = 4.dp)) }
            items(own, key = { "o" + it.pkg }) { AppLine(it, ruled[it.pkg.lowercase()], it.pkg in bypass) { open(Sheet.App(it.pkg, it.label)) } }
            if (rest.isNotEmpty()) item { Kicker("Остальные · по правилам сайтов", Modifier.padding(start = 12.dp, top = 12.dp, bottom = 4.dp)) }
            items(rest, key = { "r" + it.pkg }) { AppLine(it, null, false) { open(Sheet.App(it.pkg, it.label)) } }
        }
        item { Spacer(Modifier.height(24.dp)) }
    }
}

@Composable
private fun AppLine(app: AppInfo, rule: RuleRow?, bypass: Boolean, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().heightIn(min = 60.dp).clip(RoundedCornerShape(16.dp)).clickable(onClick = onClick)
            .padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        AppIcon(app.pkg, app.label)
        Column(Modifier.weight(1f).padding(horizontal = 14.dp, vertical = 8.dp)) {
            Text(app.label, style = T.body, maxLines = 1)
            Text(app.pkg, style = T.tiny.copy(fontSize = 11.5.sp), maxLines = 1)
        }
        when {
            bypass -> Text("мимо VPN", style = T.small.copy(color = C.Good))
            rule != null -> Box(Modifier.alpha(if (rule.enabled) 1f else 0.42f)) { Pill(rule.action) }
        }
    }
}

@Composable
fun Search(text: String, hint: String, modifier: Modifier = Modifier, change: (String) -> Unit) {
    BasicTextField(
        text, change, singleLine = true, textStyle = T.body, cursorBrush = SolidColor(C.Accent),
        modifier = modifier.padding(vertical = 6.dp).fillMaxWidth().heightIn(min = 48.dp).clip(RoundedCornerShape(24.dp))
            .background(C.Sunk).border(1.dp, Color(0x1AEFE8F5), RoundedCornerShape(24.dp)).padding(horizontal = 18.dp, vertical = 13.dp),
        decorationBox = { inner ->
            if (text.isEmpty()) Text(hint, style = T.body.copy(color = C.N500))
            inner()
        },
    )
}

/**
 * One app's way: a tap applies, and there is nothing to save. «Как решат
 * правила сайтов» takes its rule away.
 */
@Composable
fun AppSheet(frame: Frame, pkg: String, label: String, c: Commands, close: () -> Unit) {
    val context = LocalContext.current
    val at = frame.rules.indexOfFirst { it.app.equals(pkg, ignoreCase = true) }
    val rule = frame.rules.getOrNull(at)
    var bypass by remember { mutableStateOf(pkg in Bypass.read(context)) }
    var touched by remember { mutableStateOf(false) }
    BlissSheet(close) {
        Row(Modifier.padding(horizontal = 20.dp, vertical = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            AppIcon(pkg, label, 48.dp)
            Column(Modifier.padding(start = 14.dp)) {
                Text(label, style = T.sheet, maxLines = 1)
                Text(
                    when {
                        bypass -> "мимо VPN — bliss его не видит"
                        touched && rule != null -> "сохранено — правило №${at + 1}"
                        touched -> "решают правила сайтов"
                        else -> pkg
                    },
                    style = T.small.copy(color = if (touched || bypass) C.Good else C.N500), maxLines = 1,
                )
            }
        }
        val current = rule?.action
        for ((action, text) in listOf(null to "Как решат правила сайтов", "proxy" to "Через прокси",
            "direct" to "Напрямую", "block" to "Не пускать в сеть")) {
            val lit = current == action
            Row(
                Modifier.padding(horizontal = 8.dp).fillMaxWidth().heightIn(min = 54.dp).clip(RoundedCornerShape(14.dp))
                    .background(if (lit) C.Accent.copy(alpha = 0.08f) else Color.Transparent)
                    .clickable {
                        c("app_route", "package" to pkg, "label" to label, "action" to action)
                        touched = true
                    }.padding(horizontal = 14.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Box(Modifier.size(10.dp).clip(RoundedCornerShape(5.dp))
                    .background(if (action == null) C.N500 else actionColor(action)))
                Text(text, style = T.body, modifier = Modifier.weight(1f).padding(start = 14.dp))
                if (lit) Ico(Icons.Check, C.Accent, 20.dp)
            }
        }
        if (current == "direct" || bypass) {
            Row(
                Modifier.padding(horizontal = 16.dp, vertical = 10.dp).fillMaxWidth().clip(RoundedCornerShape(16.dp))
                    .background(C.Tint).padding(14.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f).padding(end = 12.dp)) {
                    Text("Полностью мимо VPN", style = T.body)
                    Text("Для банков, которые не работают при включённом VPN. VPN переподключится на секунду. " +
                        "При «Постоянном VPN» с блокировкой у такого приложения сети не будет вовсе.",
                        style = T.tiny.copy(fontSize = 12.sp))
                }
                Toggle(bypass, color = C.Good) {
                    bypass = it
                    Bypass.set(context, pkg, it)
                }
            }
        }
    }
}

// --- the list ----------------------------------------------------------------

@Composable
private fun RuleList(frame: Frame, c: Commands, open: (Sheet) -> Unit) {
    LazyColumn(Modifier.fillMaxSize().padding(horizontal = 12.dp)) {
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
            val geo = "geosite" in w || "geoip" in w
            Column(Modifier.padding(bottom = 8.dp).fillMaxWidth().clip(RoundedCornerShape(16.dp)).background(Color(0x12CBBAF4))
                .border(1.dp, Color(0x2ECBBAF4), RoundedCornerShape(16.dp)).padding(12.dp)) {
                Text(w, style = T.body.copy(fontSize = 13.sp, color = C.Violet300))
                if (geo && frame.geo.site == null) {
                    Text(if (frame.geo.fetching != null) "Скачиваю гео-файлы… ${bytes(frame.geo.fetching)}" else "Скачать гео-файлы",
                        style = T.body.copy(fontSize = 14.sp, color = C.Accent),
                        modifier = Modifier.padding(top = 6.dp).clip(RoundedCornerShape(12.dp))
                            .clickable { c("geo_update", "source" to null) }.padding(vertical = 6.dp))
                }
            }
        }
        itemsIndexed(frame.rules) { i, r ->
            Row(
                Modifier.padding(bottom = 4.dp).fillMaxWidth().heightIn(min = 58.dp).clip(RoundedCornerShape(16.dp))
                    .background(Color(0x08EFE8F5)).clickable { open(Sheet.RuleEdit(i)) }.padding(horizontal = 12.dp),
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
            Text("Сайты и категории видны правилам, потому что bliss сам отвечает телефону на DNS. " +
                "Если в Android включён «Частный DNS» с именем сервера, телефон спрашивает его напрямую, и правила видят только адреса.",
                style = T.tiny.copy(fontSize = 12.5.sp), modifier = Modifier.padding(start = 12.dp, top = 18.dp, end = 12.dp))
        }
        item { Spacer(Modifier.height(96.dp)) }
    }
}

// --- the editor ----------------------------------------------------------------

/** What kind of condition the add row is asking for. */
private enum class Adding { None, Site, App, Category, Address }

/**
 * A rule's editor: a name, where it sends, and its conditions as chips
 * added by kind — or, in «Текстом», the desktop's box, one to a line, in the
 * same words (`geosite:google`, `app:org.telegram.messenger`, `port:443`).
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun RuleEditor(frame: Frame, index: Int?, c: Commands, close: () -> Unit) {
    val old = index?.let { frame.rules.getOrNull(it) }
    var name by remember { mutableStateOf(if (old?.named == true) old.name else "") }
    var action by remember { mutableStateOf(old?.action?.takeIf { it != "vpn" } ?: "proxy") }
    val lines = remember { mutableStateListOf<String>().apply { addAll(old?.text?.lines()?.filter { it.isNotBlank() }.orEmpty()) } }
    var asText by remember { mutableStateOf(false) }
    var text by remember { mutableStateOf("") }
    var adding by remember { mutableStateOf(Adding.None) }
    DisposableEffect(Unit) {
        c("geo_names", "open" to true)
        onDispose { c("geo_names", "open" to false) }
    }
    BlissSheet(close) {
        Row(Modifier.padding(start = 20.dp, end = 8.dp, top = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(if (index == null) "Новое правило" else "Правило №${index + 1}", style = T.sheet, modifier = Modifier.weight(1f))
            if (index != null) {
                IconBtn(Icons.Up, C.N400, "Выше") { if (index > 0) { c("move_rule", "from" to index, "to" to index - 1); close() } }
                IconBtn(Icons.Down, C.N400, "Ниже") { if (index < frame.rules.size - 1) { c("move_rule", "from" to index, "to" to index + 1); close() } }
                IconBtn(Icons.Trash, C.BadText, "Удалить") { c("delete_rule", "index" to index); close() }
            }
        }
        LazyColumn(Modifier.heightIn(max = 470.dp).padding(horizontal = 16.dp)) {
            item {
                Field(name, "Название, например «Работа»") { name = it }
                Row(Modifier.padding(vertical = 10.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    for (a in listOf("proxy", "direct", "block")) Chip(actionWord(a), action == a, color = actionColor(a)) { action = a }
                }
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Kicker("Условия — хватит любого", Modifier.weight(1f))
                    Text(if (asText) "Чипами" else "Текстом", style = T.small.copy(color = C.Accent),
                        modifier = Modifier.clip(RoundedCornerShape(12.dp)).clickable {
                            if (asText) { lines.clear(); lines.addAll(text.lines().map { it.trim() }.filter { it.isNotEmpty() }) }
                            else text = lines.joinToString("\n")
                            asText = !asText
                        }.padding(horizontal = 10.dp, vertical = 8.dp))
                }
            }
            if (asText) item {
                BasicTextField(
                    text, { text = it }, textStyle = T.body.copy(fontSize = 14.sp), cursorBrush = SolidColor(C.Accent),
                    modifier = Modifier.padding(top = 6.dp).fillMaxWidth().heightIn(min = 140.dp).clip(RoundedCornerShape(16.dp))
                        .background(C.Sunk).border(1.dp, Color(0x1AEFE8F5), RoundedCornerShape(16.dp)).padding(14.dp),
                    decorationBox = { inner ->
                        if (text.isEmpty()) Text("example.com\ngeosite:google\napp:org.telegram.messenger\n10.0.0.0/8\nport:443",
                            style = T.body.copy(fontSize = 14.sp, color = C.N600))
                        inner()
                    },
                )
            } else {
                item {
                    FlowRow(Modifier.padding(top = 6.dp), horizontalArrangement = Arrangement.spacedBy(6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        for (line in lines.toList()) Condition(line) { lines.remove(line) }
                        if (lines.isEmpty()) Text("Пока ни одного — добавьте ниже.", style = T.small)
                    }
                    Row(Modifier.padding(top = 12.dp).horizontalScroll(rememberScrollState()), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                        for ((kind, word) in listOf(Adding.Site to "+ Сайт", Adding.App to "+ Приложение",
                            Adding.Category to "+ Категория", Adding.Address to "+ Адрес или порт")) {
                            Chip(word, adding == kind) { adding = if (adding == kind) Adding.None else kind }
                        }
                    }
                }
                item {
                    val add = { value: String -> if (value.isNotBlank() && value !in lines) lines.add(value.trim()); adding = Adding.None }
                    when (adding) {
                        Adding.None -> {}
                        Adding.Site -> Entry("example.com — и всё под ним", add)
                        Adding.Address -> Entry("1.2.3.4, 10.0.0.0/8 или port:443", add)
                        Adding.App -> AppPicker { add("app:$it") }
                        Adding.Category -> CategoryPicker(frame, c, add)
                    }
                }
            }
        }
        Row(Modifier.padding(horizontal = 16.dp, vertical = 12.dp), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Btn("Отмена", accent = false, modifier = Modifier.weight(1f), onClick = close)
            val conditions = if (asText) text else lines.joinToString("\n")
            Btn("Сохранить", modifier = Modifier.weight(1.4f), enabled = conditions.isNotBlank()) {
                c("save_rule", "index" to index, "name" to name, "action" to action, "text" to conditions)
                close()
            }
        }
    }
}

@Composable
private fun Condition(line: String, remove: () -> Unit) {
    val context = LocalContext.current
    val shown = when {
        line.startsWith("app:") -> "📱 " + Apps.label(context, line.removePrefix("app:"))
        line.startsWith("process:") -> "📱 " + Apps.label(context, line.removePrefix("process:"))
        line.startsWith("geosite:") || line.startsWith("geoip:") -> "🗂 " + line
        else -> line
    }
    Row(
        Modifier.clip(RoundedCornerShape(16.dp)).background(C.Accent.copy(alpha = 0.10f))
            .border(1.dp, C.Accent.copy(alpha = 0.22f), RoundedCornerShape(16.dp)).clickable(onClick = remove)
            .padding(start = 12.dp, end = 8.dp, top = 7.dp, bottom = 7.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(shown, style = T.body.copy(fontSize = 13.5.sp), maxLines = 1)
        Spacer(Modifier.width(6.dp))
        Ico(Icons.Close, C.N400, 14.dp)
    }
}

@Composable
private fun Field(value: String, hint: String, change: (String) -> Unit) {
    BasicTextField(
        value, change, singleLine = true, textStyle = T.body, cursorBrush = SolidColor(C.Accent),
        modifier = Modifier.padding(top = 8.dp).fillMaxWidth().heightIn(min = 50.dp).clip(RoundedCornerShape(16.dp))
            .background(C.Sunk).border(1.dp, Color(0x1AEFE8F5), RoundedCornerShape(16.dp)).padding(horizontal = 16.dp, vertical = 14.dp),
        decorationBox = { inner ->
            if (value.isEmpty()) Text(hint, style = T.body.copy(color = C.N500))
            inner()
        },
    )
}

@Composable
private fun Entry(hint: String, add: (String) -> Unit) {
    var value by remember { mutableStateOf("") }
    Row(Modifier.padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        Box(Modifier.weight(1f)) { Field(value, hint) { value = it } }
        Spacer(Modifier.width(8.dp))
        Btn("Добавить", enabled = value.isNotBlank(), modifier = Modifier.padding(top = 8.dp)) { add(value) }
    }
}

@Composable
private fun AppPicker(pick: (String) -> Unit) {
    val context = LocalContext.current
    val apps by produceState<List<AppInfo>?>(null) { value = Apps.load(context) }
    var query by remember { mutableStateOf("") }
    Column(Modifier.padding(top = 8.dp)) {
        Search(query, "Найти приложение") { query = it }
        val found = apps.orEmpty().filter { query.isBlank() || it.label.contains(query, true) }.take(40)
        for (app in found) {
            Row(
                Modifier.fillMaxWidth().heightIn(min = 52.dp).clip(RoundedCornerShape(14.dp)).clickable { pick(app.pkg) }
                    .padding(horizontal = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                AppIcon(app.pkg, app.label, 32.dp)
                Text(app.label, style = T.body, maxLines = 1, modifier = Modifier.padding(start = 12.dp))
            }
        }
    }
}

@Composable
private fun CategoryPicker(frame: Frame, c: Commands, pick: (String) -> Unit) {
    val names = frame.categories
    var query by remember { mutableStateOf("") }
    Column(Modifier.padding(top = 8.dp)) {
        if (names.isNullOrEmpty()) {
            Text("Категории берутся из гео-файлов, а их на телефоне пока нет.", style = T.small)
            Btn(if (frame.geo.fetching != null) "Скачиваю… ${bytes(frame.geo.fetching)}" else "Скачать гео-файлы",
                enabled = frame.geo.fetching == null, modifier = Modifier.padding(top = 8.dp)) { c("geo_update", "source" to null) }
            return@Column
        }
        Search(query, "google, youtube, ru, category-ads-all…") { query = it }
        val q = query.trim().lowercase()
        // The most likely first: an exact name, then names that start with it.
        val found = names.asSequence().filter { q.isEmpty() || it.substringAfter(':').contains(q) }
            .sortedBy { val n = it.substringAfter(':'); if (n == q) 0 else if (n.startsWith(q)) 1 else 2 }
            .take(60).toList()
        for (name in found) {
            Text(name, style = T.body.copy(fontSize = 14.sp), modifier = Modifier.fillMaxWidth().clip(RoundedCornerShape(12.dp))
                .clickable { pick(name) }.padding(horizontal = 10.dp, vertical = 11.dp))
        }
    }
}
