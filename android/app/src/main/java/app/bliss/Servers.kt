package app.bliss

import android.content.ClipboardManager
import android.content.Context
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun Servers(frame: Frame, c: Commands, add: () -> Unit, menu: (Int) -> Unit, group: (String) -> Unit) {
    val probing = frame.servers.any { it.ping == "probing" }
    val checking = frame.servers.any { it.check == "probing" }
    val starred = frame.servers.withIndex().filter { it.value.favorite }
    val own = frame.servers.withIndex().filter { it.value.group == null && !it.value.favorite }
    @Composable
    fun Line(i: Int, s: ServerRow) = ServerLine(
        s, i == frame.current,
        onClick = { c("select", "index" to i, "key" to s.key) },
        onMore = { menu(i) },
        onStar = { c("favorite", "key" to s.key, "on" to !s.favorite) },
    )
    Box(Modifier.fillMaxSize().statusBarsPadding()) {
        PullToRefreshBox(isRefreshing = probing, onRefresh = { c("ping") }, modifier = Modifier.fillMaxSize()) {
            LazyColumn(Modifier.fillMaxSize().padding(horizontal = 12.dp)) {
                item {
                    Row(Modifier.padding(start = 4.dp, top = 8.dp, bottom = 10.dp), verticalAlignment = Alignment.CenterVertically) {
                        Column(Modifier.weight(1f)) {
                            Text("Серверы", style = T.title)
                            Text("${plural(frame.servers.size.toLong(), "сервер", "сервера", "серверов")} · потяните вниз — отклик",
                                style = T.small)
                        }
                        IconBtn(Icons.Pulse, C.Accent, "Проверить отклик всех") { c("ping") }
                        // The real check: a page loaded through each server, not a handshake.
                        IconBtn(Icons.Shield, if (checking) C.Violet else C.Accent, "Проверить доступ через все") { c("check", "index" to null) }
                    }
                }
                if (frame.servers.isEmpty() && frame.groups.isEmpty()) item { Empty(add) }
                if (frame.servers.size > 1) item { AutoLine(frame, c) }
                // Starred servers go first, wherever they came from, and are
                // not shown a second time in their own group.
                if (starred.isNotEmpty()) {
                    item { Kicker("Избранное", Modifier.padding(start = 12.dp, top = 6.dp, bottom = 4.dp)) }
                    items(starred, key = { "f${it.index}:${it.value.key}" }) { (i, s) -> Line(i, s) }
                }
                if (own.isNotEmpty()) item { Kicker("Свои", Modifier.padding(start = 12.dp, top = 6.dp, bottom = 4.dp)) }
                items(own, key = { "s${it.index}:${it.value.name}" }) { (i, s) -> Line(i, s) }
                for (g in frame.groups) {
                    item(key = "g" + g.id) { GroupHead(g, frame.servers.count { it.group == g.id }, c, group) }
                    val rows = frame.servers.withIndex().filter { it.value.group == g.id && !it.value.favorite }
                    items(rows, key = { "g${g.id}:${it.index}" }) { (i, s) -> Line(i, s) }
                }
                item { Spacer(Modifier.height(96.dp)) }
            }
        }
        Row(
            Modifier.align(Alignment.BottomEnd).padding(16.dp).height(56.dp).clip(RoundedCornerShape(28.dp))
                .background(Color(0xFF2A1F37)).border(1.dp, C.Accent.copy(alpha = 0.5f), RoundedCornerShape(28.dp))
                .clickable(onClick = add).padding(start = 18.dp, end = 22.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Ico(Icons.Plus, C.Accent)
            Spacer(Modifier.width(10.dp))
            Text("Добавить", style = T.body.copy(fontWeight = FontWeight.Bold, color = C.Accent))
        }
    }
}

@Composable
private fun Empty(add: () -> Unit) {
    Column(Modifier.fillMaxWidth().padding(top = 8.dp).card().padding(20.dp), horizontalAlignment = Alignment.CenterHorizontally) {
        Image(painterResource(R.drawable.cat), null, Modifier.size(220.dp, 190.dp))
        Text("Здесь пока тихо", style = T.sheet)
        Text("Добавьте сервер — ссылкой или подпиской. Потом подключайтесь одной кнопкой на главном экране.",
            style = T.body.copy(fontSize = 14.sp, color = C.N400), modifier = Modifier.padding(top = 4.dp, bottom = 12.dp))
        Btn("Добавить", icon = Icons.Plus, modifier = Modifier.fillMaxWidth(), onClick = add)
    }
}

/**
 * «Автовыбор»: the fastest server a page loads through, and the next one
 * when it stops answering — measured on a start, on a failure and when the
 * screen comes on, never on a timer. A server picked by hand turns it off.
 */
@Composable
fun AutoLine(frame: Frame, c: Commands) {
    val shape = RoundedCornerShape(16.dp)
    val now = frame.current?.let { frame.servers.getOrNull(it) }
    Row(
        Modifier.padding(bottom = 6.dp).fillMaxWidth().heightIn(min = 60.dp).clip(shape)
            .background(if (frame.auto) C.Accent.copy(alpha = 0.10f) else C.Tint)
            .border(1.dp, if (frame.auto) C.Accent.copy(alpha = 0.24f) else Color(0x14EFE8F5), shape)
            .clickable { c("auto", "on" to !frame.auto) }.padding(start = 14.dp, end = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Ico(Icons.Pulse, if (frame.auto) C.Accent else C.N400, 20.dp)
        Column(Modifier.weight(1f).padding(horizontal = 12.dp, vertical = 8.dp)) {
            Text("Автовыбор", style = T.body.copy(fontWeight = FontWeight.Bold))
            Text(
                if (frame.auto && now != null) "сейчас ${now.name} — сменю, если перестанет отвечать"
                else "самый быстрый, кто отвечает, и следующий, если он замолчит",
                style = T.tiny.copy(fontSize = 12.5.sp), maxLines = 2,
            )
        }
        Toggle(frame.auto) { c("auto", "on" to it) }
    }
}

@Composable
fun ServerLine(s: ServerRow, current: Boolean, onClick: () -> Unit, onMore: (() -> Unit)?, onStar: (() -> Unit)? = null) {
    val shape = RoundedCornerShape(16.dp)
    Row(
        Modifier.fillMaxWidth().heightIn(min = 60.dp).clip(shape)
            .background(if (current) C.Accent.copy(alpha = 0.10f) else Color.Transparent)
            .border(1.dp, if (current) C.Accent.copy(alpha = 0.24f) else Color.Transparent, shape)
            .clickable(onClick = onClick).padding(start = 14.dp, end = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f).padding(vertical = 8.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                if (current) { Ico(Icons.HeartFilled, C.Accent, 14.dp); Spacer(Modifier.width(7.dp)) }
                // Where the star cannot be tapped, it is still shown.
                if (s.favorite && onStar == null) { Ico(Icons.StarFilled, C.Accent, 14.dp); Spacer(Modifier.width(7.dp)) }
                Text(s.name, style = T.body, maxLines = 1)
            }
            Text(listOf(s.kind, s.address).filter { it.isNotEmpty() }.joinToString(" · "), style = T.tiny.copy(fontSize = 12.5.sp), maxLines = 1)
        }
        // The real check above the handshake's delay when there is one: the
        // first says the server takes you somewhere, the second only that it answers.
        Column(Modifier.padding(horizontal = 8.dp), horizontalAlignment = Alignment.End) {
            val check = checkText(s)
            if (check.isNotEmpty()) {
                Text(check, style = T.small.copy(color = checkColor(s)))
                Text("отклик ${pingText(s)}", style = T.tiny.copy(fontSize = 11.5.sp, color = pingColor(s).copy(alpha = 0.8f)))
            } else Text(pingText(s), style = T.small.copy(color = pingColor(s)))
        }
        if (onStar != null) IconBtn(if (s.favorite) Icons.StarFilled else Icons.Star,
            if (s.favorite) C.Accent else C.N500, if (s.favorite) "Убрать из избранного" else "В избранное", onStar)
        if (onMore != null) IconBtn(Icons.More, C.N500, "Действия с сервером", onMore)
    }
}

@Composable
private fun GroupHead(g: GroupRow, count: Int, c: Commands, more: (String) -> Unit) {
    Row(Modifier.padding(top = 10.dp).fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f).padding(start = 10.dp, top = 8.dp, bottom = 8.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Ico(Icons.Rss, C.Accent, 15.dp)
                Spacer(Modifier.width(8.dp))
                Text(g.name, style = T.body.copy(fontWeight = FontWeight.Bold), maxLines = 1)
                Spacer(Modifier.width(8.dp))
                Text(plural(count.toLong(), "сервер", "сервера", "серверов"), style = T.tiny)
            }
            val line = when {
                g.fetching -> "загружаю список…"
                g.problem != null -> g.problem
                g.updated != null -> "обновлена ${ago(g.updated)}"
                else -> "ещё не загружена"
            }
            Text(line, style = T.tiny.copy(color = if (g.problem != null) C.BadText else if (g.fetching) C.Violet300 else C.N500),
                modifier = Modifier.padding(start = 23.dp, top = 2.dp), maxLines = 2)
            if (g.total != null && g.used != null) {
                Row(Modifier.padding(start = 23.dp, top = 6.dp), verticalAlignment = Alignment.CenterVertically) {
                    val part = (g.used.toFloat() / g.total).coerceIn(0f, 1f)
                    Box(Modifier.size(110.dp, 4.dp).clip(RoundedCornerShape(2.dp)).background(Color(0x14EFE8F5))) {
                        Box(Modifier.fillMaxWidth(part).height(4.dp).background(if (part > 0.9f) C.Bad else C.Violet))
                    }
                    Spacer(Modifier.width(10.dp))
                    Text("${bytes(g.used)} из ${bytes(g.total)}", style = T.tiny)
                }
            }
        }
        IconBtn(Icons.Refresh, C.N400, "Обновить подписку") { c("refresh", "id" to g.id) }
        IconBtn(Icons.More, C.N500, "Подписка: удалить") { more(g.id) }
    }
}

fun ago(unix: Long): String {
    val s = System.currentTimeMillis() / 1000 - unix
    return when {
        s < 60 -> "только что"
        s < 3600 -> plural(s / 60, "минуту", "минуты", "минут") + " назад"
        s < 86400 -> plural(s / 3600, "час", "часа", "часов") + " назад"
        else -> plural(s / 86400, "день", "дня", "дней") + " назад"
    }
}

// --- sheets ------------------------------------------------------------------------

/**
 * What a list inside a sheet has left over at its ends — the rest of a
 * drag past the top, a fling that hit the bottom — stays with the list.
 * Handed on, it moved the sheet, which sprang back to where it stands: the
 * quick server sheet shook as the user scrolled it up and down. The sheet
 * is still dragged by its handle and its head, and closed by a tap outside
 * or by «назад».
 */
private val KeepScroll = object : androidx.compose.ui.input.nestedscroll.NestedScrollConnection {
    override fun onPostScroll(
        consumed: androidx.compose.ui.geometry.Offset,
        available: androidx.compose.ui.geometry.Offset,
        source: androidx.compose.ui.input.nestedscroll.NestedScrollSource,
    ) = available

    override suspend fun onPostFling(consumed: androidx.compose.ui.unit.Velocity, available: androidx.compose.ui.unit.Velocity) =
        available
}

/** For a scrolling list inside a `BlissSheet`: see `KeepScroll`. */
fun Modifier.scrollsInSheet(): Modifier = this.nestedScroll(KeepScroll)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun BlissSheet(close: () -> Unit, content: @Composable ColumnScope.() -> Unit) {
    ModalBottomSheet(
        onDismissRequest = close,
        sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
        containerColor = C.Surface,
        scrimColor = Color(0xA30A0610),
        shape = S.sheet,
        dragHandle = {
            Box(Modifier.padding(top = 10.dp, bottom = 4.dp).size(40.dp, 4.dp).clip(RoundedCornerShape(2.dp)).background(Color(0x38EFE8F5)))
        },
    ) {
        Column(Modifier.navigationBarsPadding().imePadding().padding(bottom = 12.dp), content = content)
    }
}

/** Quick switching, the tray menu's heir: the bunny on the sheet's edge. */
@Composable
fun ServerSheet(frame: Frame, c: Commands, close: () -> Unit, all: () -> Unit) {
    BlissSheet(close) {
        Row(Modifier.fillMaxWidth().padding(start = 20.dp), verticalAlignment = Alignment.Bottom) {
            Column(Modifier.weight(1f).padding(bottom = 10.dp)) {
                Text("Сервер", style = T.sheet)
                Text("смена — секунда без сети", style = T.small.copy(fontSize = 12.5.sp))
            }
            Image(painterResource(R.drawable.bunny), null, Modifier.padding(end = 12.dp).size(96.dp, 90.dp))
        }
        LazyColumn(Modifier.heightIn(max = 380.dp).scrollsInSheet().padding(horizontal = 8.dp)) {
            if (frame.servers.size > 1) item { AutoLine(frame, c) }
            // Starred first; the rest keep their order.
            items(frame.servers.withIndex().sortedByDescending { it.value.favorite }, key = { it.index }) { (i, s) ->
                ServerLine(s, i == frame.current, onClick = { c("select", "index" to i, "key" to s.key); close() }, onMore = null)
            }
        }
        Row(Modifier.padding(horizontal = 16.dp, vertical = 12.dp), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Btn("Отклик", accent = false, icon = Icons.Pulse, modifier = Modifier.weight(1f)) { c("ping") }
            Btn("Доступ", accent = false, icon = Icons.Shield, modifier = Modifier.weight(1f)) { c("check", "index" to null) }
            Btn("Все", modifier = Modifier.weight(0.8f), onClick = all)
        }
    }
}

@Composable
fun AddSheet(prefill: String, c: Commands, close: () -> Unit, scan: () -> Unit) {
    val context = LocalContext.current
    var text by remember { mutableStateOf(prefill) }
    val verdict = verdict(text)
    BlissSheet(close) {
        Column(Modifier.padding(horizontal = 20.dp).padding(top = 6.dp, bottom = 14.dp)) {
            Text("Добавить", style = T.sheet)
            Text("сервер или подписку — bliss поймёт по ссылке сам", style = T.small)
        }
        Row(Modifier.padding(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            AddTile(Icons.Clip, "Вставить", "из буфера обмена", Modifier.weight(1f)) {
                // Read only now, on the tap: Android shows a banner for every read.
                val clip = (context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).primaryClip
                val pasted = clip?.takeIf { it.itemCount > 0 }?.getItemAt(0)?.coerceToText(context)?.toString()
                if (!pasted.isNullOrBlank()) { c("import", "text" to pasted); close() }
            }
            AddTile(Icons.Qr, "QR-код", "камерой или с фото", Modifier.weight(1f), onClick = scan)
        }
        Text("или вставьте сюда", style = T.small.copy(color = C.N400), modifier = Modifier.padding(start = 20.dp, top = 18.dp, bottom = 8.dp))
        BasicTextField(
            text, { text = it }, singleLine = true,
            textStyle = T.body, cursorBrush = SolidColor(C.Accent),
            modifier = Modifier.padding(horizontal = 16.dp).fillMaxWidth().heightIn(min = 52.dp).clip(RoundedCornerShape(16.dp))
                .background(C.Sunk).border(if (text.isEmpty()) 1.dp else 1.5.dp,
                    if (text.isEmpty()) Color(0x1AEFE8F5) else C.Accent.copy(alpha = 0.55f), RoundedCornerShape(16.dp))
                .padding(horizontal = 16.dp, vertical = 15.dp),
            decorationBox = { inner ->
                if (text.isEmpty()) Text("vless://, trojan://, ss://… или https://", style = T.body.copy(color = C.N500))
                inner()
            },
        )
        Text(verdict.first, style = T.small.copy(color = verdict.second), modifier = Modifier.padding(start = 20.dp, top = 8.dp, end = 20.dp))
        Text("Ссылку можно и просто отправить: «Поделиться» в мессенджере → bliss.",
            style = T.tiny.copy(fontSize = 12.5.sp), modifier = Modifier.padding(start = 20.dp, top = 14.dp, end = 20.dp))
        Row(Modifier.padding(horizontal = 16.dp, vertical = 16.dp), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Btn("Отмена", accent = false, modifier = Modifier.weight(1f), onClick = close)
            Btn("Добавить", modifier = Modifier.weight(1.4f), enabled = verdict.third) { c("import", "text" to text); close() }
        }
    }
}

/** One of the add sheet's two big ways in: the clipboard, a QR code. */
@Composable
private fun AddTile(icon: androidx.compose.ui.graphics.vector.ImageVector, title: String, hint: String, modifier: Modifier, onClick: () -> Unit) {
    Row(
        modifier.height(72.dp).clip(RoundedCornerShape(20.dp)).background(C.Tint)
            .border(1.dp, Color(0x1AEFE8F5), RoundedCornerShape(20.dp))
            .clickable(onClick = onClick).padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Box(Modifier.size(44.dp).clip(RoundedCornerShape(14.dp)).background(C.Accent.copy(alpha = 0.12f)), contentAlignment = Alignment.Center) {
            Ico(icon, C.Accent)
        }
        Column(Modifier.padding(start = 12.dp)) {
            Text(title, style = T.body.copy(fontWeight = FontWeight.Bold), maxLines = 1)
            Text(hint, style = T.tiny.copy(fontSize = 12.sp), maxLines = 2)
        }
    }
}

/** What the text looks like, said by kind and never quoting it back. */
fun verdict(text: String): Triple<String, Color, Boolean> {
    val t = text.trim().lowercase()
    val m = Regex("^(vless|vmess|trojan|ss|socks|hysteria2|hy2|wireguard)://").find(t)
    return when {
        t.isEmpty() -> Triple("ссылка видна только здесь — в списке будет лишь имя", C.N500, false)
        m != null -> Triple("Сервер ${m.groupValues[1].uppercase()} — ляжет в «Свои»", C.Good, true)
        t.startsWith("https://") || t.startsWith("http://") -> Triple("Подписка — загружу список серверов", C.Good, true)
        t.startsWith("tuic://") -> Triple("tuic:// движок xray не знает — такой сервер не добавить", C.BadText, false)
        Regex("^[a-z0-9]+://").containsMatchIn(t) -> Triple("Такой схемы bliss не знает", C.BadText, false)
        else -> Triple("Не похоже на ссылку на сервер или подписку", C.N500, false)
    }
}

@Composable
fun ServerMenu(frame: Frame, index: Int, c: Commands, close: () -> Unit) {
    // The server the menu was opened on, followed by its key: a list
    // refreshed while the menu is up moves the places.
    val key = remember { frame.servers.getOrNull(index)?.key } ?: return close()
    val at = frame.servers.indexOfFirst { it.key == key }
    val s = frame.servers.getOrNull(at) ?: return close()
    BlissSheet(close) {
        Column(Modifier.padding(horizontal = 20.dp, vertical = 8.dp)) {
            Text(s.name, style = T.sheet, maxLines = 1)
            Text(listOf(s.detail, s.address).filter { it.isNotEmpty() }.joinToString(" · "), style = T.small, maxLines = 2)
        }
        MenuRow(Icons.Check, "Подключаться через него", C.N300) { c("select", "index" to at, "key" to key); close() }
        MenuRow(if (s.favorite) Icons.StarFilled else Icons.Star, if (s.favorite) "Убрать из избранного" else "В избранное", C.N300) {
            c("favorite", "key" to key, "on" to !s.favorite); close()
        }
        MenuRow(Icons.Shield, "Проверить доступ через него", C.N300) { c("check", "index" to at, "key" to key); close() }
        MenuRow(Icons.Trash, "Удалить", C.BadText) { c("delete", "index" to at, "key" to key); close() }
    }
}

@Composable
fun GroupMenu(frame: Frame, id: String, c: Commands, close: () -> Unit) {
    val g = frame.groups.firstOrNull { it.id == id } ?: return close()
    BlissSheet(close) {
        Column(Modifier.padding(horizontal = 20.dp, vertical = 8.dp)) {
            Text(g.name, style = T.sheet, maxLines = 1)
            Text(g.host, style = T.small)
        }
        MenuRow(Icons.Refresh, "Обновить", C.N300) { c("refresh", "id" to id); close() }
        MenuRow(Icons.Trash, "Удалить подписку и её серверы", C.BadText) { c("delete_group", "id" to id); close() }
    }
}

@Composable
fun MenuRow(icon: androidx.compose.ui.graphics.vector.ImageVector, text: String, color: Color, onClick: () -> Unit) {
    Row(
        Modifier.padding(horizontal = 8.dp).fillMaxWidth().heightIn(min = 56.dp).clip(RoundedCornerShape(14.dp))
            .clickable(onClick = onClick).padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Ico(icon, color)
        Spacer(Modifier.width(16.dp))
        Text(text, style = T.body.copy(color = color))
    }
}

/**
 * Android did not give bliss the VPN. Pressed «Отмена», it is a word and
 * another try; a dialog that closed by itself is nearly always another app
 * holding «Постоянный VPN» (Samsung's Secure Wi-Fi among them), or bliss
 * running in a work profile or Secure Folder, where Android lets no VPN be
 * asked for. Its settings page is one tap away.
 */
@Composable
fun RefusedSheet(fast: Boolean, close: () -> Unit, again: () -> Unit) {
    val context = LocalContext.current
    BlissSheet(close) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 24.dp, vertical = 10.dp)) {
            Text(if (fast) "Android не показал разрешение" else "VPN не разрешён", style = T.sheet)
            Text(
                if (fast) "Окно разрешения закрылось само. Так бывает, когда у другого приложения включён «Постоянный VPN» " +
                    "(на Samsung — и встроенный Secure Wi-Fi), или bliss открыт в рабочем профиле или Защищённой папке.\n\n" +
                    "Откройте настройки VPN, выключите «Постоянный VPN» у другого приложения (или выберите bliss) и попробуйте снова."
                else "Без разрешения Android bliss не может вести трафик телефона. Если нажали «Отмена» случайно — попробуйте ещё раз.",
                style = T.body.copy(fontSize = 14.sp, color = C.N400), modifier = Modifier.padding(top = 8.dp),
            )
        }
        Row(Modifier.padding(horizontal = 16.dp, vertical = 16.dp), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Btn("Настройки VPN", accent = false, modifier = Modifier.weight(1f)) {
                try { context.startActivity(android.content.Intent(android.provider.Settings.ACTION_VPN_SETTINGS)) } catch (_: Exception) { }
            }
            Btn("Ещё раз", modifier = Modifier.weight(1f), onClick = again)
        }
    }
}

/** Said before Android's own dialog, so the person knows which button to press. */
@Composable
fun PermissionSheet(close: () -> Unit, go: () -> Unit) {
    BlissSheet(close) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 24.dp, vertical = 10.dp), horizontalAlignment = Alignment.CenterHorizontally) {
            Ring("busy", 76.dp) {}
            Text("Android спросит разрешение", style = T.sheet, modifier = Modifier.padding(top = 14.dp))
            Text("bliss ведёт трафик как VPN-сервис — только так через него идёт весь телефон. " +
                "Нажмите «OK» в окне Android. Это один раз, потом в строке состояния будет ключик.",
                style = T.body.copy(fontSize = 14.sp, color = C.N400), modifier = Modifier.padding(top = 8.dp))
        }
        Row(Modifier.padding(horizontal = 16.dp, vertical = 16.dp), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Btn("Не сейчас", accent = false, modifier = Modifier.weight(1f), onClick = close)
            Btn("Понятно", modifier = Modifier.weight(1.4f), onClick = go)
        }
    }
}
