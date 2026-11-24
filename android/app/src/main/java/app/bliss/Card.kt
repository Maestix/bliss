package app.bliss

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.os.PersistableBundle
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.google.zxing.BarcodeFormat
import com.google.zxing.EncodeHintType
import com.google.zxing.qrcode.QRCodeWriter
import org.json.JSONObject

/** A link as a QR code: dark modules on white, with a quiet zone, as a camera expects. */
fun qrBitmap(text: String, size: Int = 720): Bitmap? = try {
    val matrix = QRCodeWriter().encode(text, BarcodeFormat.QR_CODE, size, size, mapOf(EncodeHintType.MARGIN to 3))
    val pixels = IntArray(size * size) { i ->
        if (matrix.get(i % size, i / size)) 0xFF140F1D.toInt() else 0xFFFFFFFF.toInt()
    }
    Bitmap.createBitmap(pixels, size, size, Bitmap.Config.ARGB_8888)
} catch (_: Exception) {
    null
}

/** On the clipboard, marked sensitive: Android 13+ then keeps it out of its preview. */
fun copySecret(context: Context, text: String) {
    val clip = ClipData.newPlainText("bliss", text)
    clip.description.extras = PersistableBundle().apply { putBoolean("android.content.extra.IS_SENSITIVE", true) }
    (context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(clip)
}

fun shareText(context: Context, text: String, title: String) {
    val send = Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, text)
    context.startActivity(Intent.createChooser(send, title))
}

/**
 * A server's or a subscription's card: every field, the secrets hidden until
 * tapped, and its link to share, copy or show as a QR code for another phone
 * or the desktop's... the other way round. The engine sends it while the
 * sheet is open, and forgets it as the sheet closes.
 */
@Composable
fun CardSheet(frame: Frame, c: Commands, close: () -> Unit, edit: (String?, JSONObject) -> Unit) {
    val context = LocalContext.current
    val done = { c("close_card"); close() }
    val card = frame.card
    var shown by remember(card?.title) { mutableStateOf(setOf<Int>()) }
    var qr by remember(card?.link) { mutableStateOf(false) }
    var note by remember { mutableStateOf("") }
    BlissSheet(done) {
        if (card == null) {
            Text("…", style = T.small, modifier = Modifier.padding(20.dp))
            return@BlissSheet
        }
        val what = if (card.key != null) "ссылку" else "адрес"
        Column(Modifier.heightIn(max = 560.dp).verticalScroll(rememberScrollState()).padding(horizontal = 20.dp)) {
            Text(card.title, style = T.sheet, maxLines = 2)
            if (qr) {
                val bitmap = remember(card.link) { qrBitmap(card.link) }
                if (bitmap != null) {
                    Box(Modifier.padding(top = 14.dp).fillMaxWidth(), contentAlignment = Alignment.Center) {
                        Image(bitmap.asImageBitmap(), "QR-код", Modifier.size(240.dp).clip(RoundedCornerShape(14.dp)))
                    }
                    Text("Отсканируйте в bliss: «Добавить» → «QR-код»", style = T.tiny,
                        modifier = Modifier.padding(top = 8.dp).fillMaxWidth())
                } else {
                    Text("Слишком длинно для QR-кода", style = T.small.copy(color = C.BadText), modifier = Modifier.padding(top = 10.dp))
                }
            }
            Column(Modifier.padding(top = 12.dp)) {
                card.rows.forEachIndexed { i, row ->
                    Row(Modifier.fillMaxWidth().padding(vertical = 5.dp)) {
                        Text(row.label, style = T.small, modifier = Modifier.width(132.dp))
                        if (row.secret && i !in shown) {
                            Text("•••••••• показать", style = T.small.copy(color = C.Accent300),
                                modifier = Modifier.clickable { shown = shown + i })
                        } else {
                            Text(row.value, style = T.small.copy(color = C.N200, fontFamily = FontFamily.Monospace, fontSize = 12.5.sp))
                        }
                    }
                }
            }
            if (note.isNotEmpty()) Text(note, style = T.tiny, modifier = Modifier.padding(top = 8.dp))
        }
        Row(Modifier.padding(horizontal = 16.dp).padding(top = 14.dp), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Btn(if (qr) "Скрыть QR" else "QR-код", accent = false, icon = Icons.Qr, modifier = Modifier.weight(1f)) { qr = !qr }
            Btn("Поделиться", icon = Icons.Share, modifier = Modifier.weight(1f)) {
                shareText(context, card.link, if (card.key != null) "Поделиться сервером" else "Поделиться подпиской")
            }
        }
        Row(Modifier.padding(horizontal = 16.dp, vertical = 12.dp), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Btn("Скопировать $what", accent = false, icon = Icons.Clip, modifier = Modifier.weight(1f)) {
                copySecret(context, card.link)
                note = if (card.key != null) "Ссылка скопирована" else "Адрес скопирован"
            }
            val form = card.form
            if (form != null) {
                Btn(if (card.manual) "Изменить" else "Копия для правки", accent = false, modifier = Modifier.weight(1f)) {
                    c("close_card")
                    edit(if (card.manual) card.key else null, form)
                }
            }
        }
    }
}

/** A new server's form: VLESS over REALITY, as most servers handed out today are. */
fun newServerForm(): JSONObject = JSONObject()
    .put("protocol", "vless").put("port", "443").put("encryption", "none").put("flow", "xtls-rprx-vision")
    .put("security", "reality").put("fingerprint", "chrome").put("transport", "tcp")

private val FINGERPRINTS = listOf("chrome" to "Chrome", "firefox" to "Firefox", "safari" to "Safari", "ios" to "iOS",
    "android" to "Android", "edge" to "Edge", "random" to "случайный")

private val SS_METHODS = listOf("2022-blake3-aes-128-gcm", "2022-blake3-aes-256-gcm", "2022-blake3-chacha20-poly1305",
    "aes-128-gcm", "aes-256-gcm", "chacha20-ietf-poly1305", "xchacha20-ietf-poly1305", "chacha20-poly1305", "none")

private var saves = 0L

/**
 * A server by hand, or one being changed (`key`): the fields its protocol
 * has, the security and the transport, the rarer ones under «Дополнительно».
 * The engine writes it as a link — the same as a pasted one — and checks it
 * by reading it back; its answer comes in `Frame.saved`.
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
fun FormSheet(frame: Frame, start: JSONObject, key: String?, c: Commands, close: () -> Unit) {
    val f = remember {
        mutableStateMapOf<String, String>().apply {
            for (k in start.keys()) if (k != "multi") put(k, start.optString(k))
        }
    }
    var multi by remember { mutableStateOf(start.optBoolean("multi")) }
    var more by remember { mutableStateOf(false) }
    var waiting by remember { mutableStateOf<Long?>(null) }
    var problem by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(frame.saved) {
        val saved = frame.saved ?: return@LaunchedEffect
        if (saved.attempt != waiting) return@LaunchedEffect
        waiting = null
        if (saved.ok) close() else problem = saved.text
    }
    fun v(k: String) = f[k] ?: ""
    fun set(k: String, value: String) {
        if (k == "protocol" && v("protocol") != value) {
            when (value) {
                "vless" -> if (v("security").isEmpty() || v("security") == "none") f["security"] = "reality"
                "vmess" -> { f["flow"] = ""; if (v("security") == "reality") f["security"] = "tls"; if (v("cipher").isEmpty()) f["cipher"] = "auto" }
                "trojan" -> { f["flow"] = ""; f["security"] = "tls" }
                "ss" -> { f["flow"] = ""; f["security"] = "none"; if (v("method").isEmpty()) f["method"] = "2022-blake3-aes-128-gcm" }
            }
        }
        f[k] = value
        problem = null
    }

    @Composable
    fun Field(label: String, k: String, hint: String = "") {
        Text(label, style = T.kicker, modifier = Modifier.padding(top = 12.dp, bottom = 6.dp))
        BasicTextField(
            v(k), { set(k, it) }, singleLine = true,
            textStyle = T.body.copy(fontSize = 14.sp, fontFamily = FontFamily.Monospace), cursorBrush = SolidColor(C.Accent),
            modifier = Modifier.fillMaxWidth().clip(RoundedCornerShape(14.dp)).background(C.Sunk)
                .border(1.dp, Color(0x1AEFE8F5), RoundedCornerShape(14.dp)).padding(horizontal = 14.dp, vertical = 12.dp),
            decorationBox = { inner ->
                if (v(k).isEmpty() && hint.isNotEmpty()) Text(hint, style = T.body.copy(fontSize = 14.sp, color = C.N600))
                inner()
            },
        )
    }

    @Composable
    fun Pills(label: String, k: String, choices: List<Pair<String, String>>) {
        Text(label, style = T.kicker, modifier = Modifier.padding(top = 12.dp, bottom = 6.dp))
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            for ((value, shown) in choices) {
                val on = v(k) == value
                Text(shown, style = T.small.copy(color = if (on) C.Accent else C.N400),
                    modifier = Modifier.clip(RoundedCornerShape(50)).background(if (on) C.Accent.copy(alpha = 0.14f) else Color.Transparent)
                        .border(1.dp, if (on) Color.Transparent else Color(0x1AEFE8F5), RoundedCornerShape(50))
                        .clickable { set(k, value) }.padding(horizontal = 12.dp, vertical = 7.dp))
            }
        }
    }

    val protocol = v("protocol")
    val streamed = protocol in listOf("vless", "vmess", "trojan", "ss")
    val security = v("security").ifEmpty { "none" }
    val transport = v("transport").ifEmpty { "tcp" }

    BlissSheet(close) {
        Column(Modifier.heightIn(max = 600.dp).verticalScroll(rememberScrollState()).padding(horizontal = 20.dp)) {
            Text(if (key != null) "Сервер" else "Новый сервер", style = T.sheet)
            problem?.let { Text(it, style = T.small.copy(color = C.BadText), modifier = Modifier.padding(top = 8.dp)) }
            Pills("ПРОТОКОЛ", "protocol", listOf("vless" to "VLESS", "vmess" to "VMess", "trojan" to "Trojan", "ss" to "Shadowsocks",
                "socks" to "SOCKS", "hysteria2" to "Hysteria2", "wireguard" to "WireGuard"))
            Field("НАЗВАНИЕ", "name", "можно не заполнять")
            Field("АДРЕС", "address", "имя или IP сервера")
            Field("ПОРТ", "port", "443")
            when (protocol) {
                "vless" -> {
                    Field("UUID", "id", "11111111-2222-…")
                    Pills("FLOW", "flow", listOf("" to "нет", "xtls-rprx-vision" to "vision", "xtls-rprx-vision-udp443" to "vision-udp443"))
                }
                "vmess" -> {
                    Field("UUID", "id", "11111111-2222-…")
                    Pills("ШИФР", "cipher", listOf("auto", "aes-128-gcm", "chacha20-poly1305", "none", "zero").map { it to it })
                }
                "trojan" -> Field("ПАРОЛЬ", "password")
                "ss" -> { Pills("ШИФР", "method", SS_METHODS.map { it to it }); Field("ПАРОЛЬ", "password") }
                "socks" -> { Field("ПОЛЬЗОВАТЕЛЬ", "user", "без входа — пусто"); Field("ПАРОЛЬ", "password") }
                "hysteria2" -> {
                    Field("ПАРОЛЬ (AUTH)", "password")
                    Field("SNI", "sni", "по умолчанию — адрес")
                    Field("ПАРОЛЬ SALAMANDER", "obfs", "без обфускации — пусто")
                    Field("ПРЫЖКИ ПО ПОРТАМ", "hop", "20000-30000")
                }
                "wireguard" -> {
                    Field("ЗАКРЫТЫЙ КЛЮЧ", "private_key", "base64")
                    Field("ОТКРЫТЫЙ КЛЮЧ СЕРВЕРА", "peer_key", "base64")
                    Field("АДРЕСА В ТУННЕЛЕ", "local_addresses", "10.0.0.2/32")
                }
            }
            if (streamed) {
                Pills("ЗАЩИТА", "security", listOf("none" to "нет", "tls" to "TLS", "reality" to "REALITY"))
                if (security == "tls") {
                    Field("SNI", "sni", "по умолчанию — адрес")
                    Pills("ОТПЕЧАТОК", "fingerprint", FINGERPRINTS)
                }
                if (security == "reality") {
                    Field("SNI — САЙТ ДЛЯ МАСКИРОВКИ", "sni", "www.example.com")
                    Field("ОТКРЫТЫЙ КЛЮЧ (PBK)", "public_key")
                    Field("SHORT ID", "short_id", "можно пусто")
                    Pills("ОТПЕЧАТОК", "fingerprint", FINGERPRINTS)
                }
                Pills("ТРАНСПОРТ", "transport", listOf("tcp" to "TCP", "ws" to "WebSocket", "httpupgrade" to "HTTPUpgrade",
                    "xhttp" to "XHTTP", "grpc" to "gRPC", "kcp" to "mKCP"))
                when (transport) {
                    "ws", "httpupgrade", "xhttp" -> {
                        Field("HOST", "host")
                        Field("ПУТЬ", "path", "/")
                        if (transport == "xhttp") Pills("РЕЖИМ", "mode", listOf("auto", "packet-up", "stream-up", "stream-one").map { it to it })
                    }
                    "grpc" -> Field("SERVICENAME", "service_name")
                    "kcp" -> {
                        Pills("МАСКИРОВКА", "header", listOf("none", "srtp", "utp", "wechat-video", "dtls", "wireguard").map { it to it })
                        Field("SEED", "seed")
                    }
                }
            }
            Row(Modifier.padding(top = 14.dp).clip(RoundedCornerShape(12.dp)).clickable { more = !more }.padding(vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically) {
                Ico(if (more) Icons.Up else Icons.Down, C.N400, size = 18.dp)
                Text("Дополнительно", style = T.small, modifier = Modifier.padding(start = 8.dp))
            }
            if (more) {
                when (protocol) {
                    "vless" -> Field("ШИФРОВАНИЕ (VLESS ENCRYPTION)", "encryption", "none")
                    "wireguard" -> { Field("ОБЩИЙ КЛЮЧ (PSK)", "pre_shared_key"); Field("RESERVED", "reserved", "0,0,0"); Field("MTU", "mtu", "1420") }
                    "hysteria2" -> { Field("ALPN", "alpn"); Field("ЗАКРЕПЛЁННЫЙ СЕРТИФИКАТ (SHA-256)", "pinned") }
                }
                if (streamed && security == "tls") {
                    Field("ALPN", "alpn", "h2,http/1.1")
                    Field("ЗАКРЕПЛЁННЫЙ СЕРТИФИКАТ (SHA-256)", "pinned")
                    Field("ИМЕНА СЕРТИФИКАТА (VCN)", "verify_names")
                    Field("ECH", "ech")
                }
                if (streamed && security == "reality") { Field("SPIDERX", "spider_x"); Field("ML-DSA-65 (PQV)", "mldsa65") }
                if (streamed && transport == "xhttp") Field("EXTRA (JSON)", "extra")
                if (streamed && transport == "grpc") {
                    Field("AUTHORITY", "authority")
                    Row(Modifier.padding(top = 10.dp).clickable { multi = !multi }, verticalAlignment = Alignment.CenterVertically) {
                        Toggle(multi) { multi = it }
                        Text("Режим multi", style = T.small, modifier = Modifier.padding(start = 10.dp))
                    }
                }
                if (streamed && transport == "kcp") Field("MTU", "mtu")
                if (streamed && transport == "tcp") {
                    Pills("МАСКИРОВКА TCP", "header", listOf("none" to "нет", "http" to "HTTP"))
                    if (v("header") == "http") { Field("HOST", "host"); Field("ПУТЬ", "path", "/") }
                }
                Field("FINALMASK (FM, JSON)", "finalmask")
            }
            Text("Сервер сохранится ссылкой — такой же, как вставленная", style = T.tiny, modifier = Modifier.padding(top = 14.dp))
        }
        Row(Modifier.padding(horizontal = 16.dp, vertical = 14.dp), horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Btn("Отмена", accent = false, modifier = Modifier.weight(1f), onClick = close)
            Btn(if (waiting != null) "Сохраняю…" else "Сохранить", modifier = Modifier.weight(1.4f), enabled = waiting == null) {
                val form = JSONObject()
                for ((k, value) in f) form.put(k, value)
                form.put("multi", multi)
                saves += 1
                waiting = saves
                c("save_server", "key" to key, "form" to form, "attempt" to saves)
            }
        }
    }
}
