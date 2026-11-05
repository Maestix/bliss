package app.bliss

import android.content.Intent
import android.provider.Settings as AndroidSettings
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.runtime.setValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

@Composable
fun Settings(frame: Frame, c: Commands, openTraffic: () -> Unit, openNetwork: (String, String) -> Unit) {
    val context = LocalContext.current
    Column(
        Modifier.fillMaxSize().statusBarsPadding().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp),
    ) {
        Text("Настройки", style = T.title, modifier = Modifier.padding(top = 8.dp, bottom = 12.dp))

        Section("Что идёт через bliss") {
            Line("Весь телефон", "VPN-сервис Android: каждое приложение, UDP, звонки и игры. " +
                "IPv6 пока отклоняется, и приложения уходят на IPv4 — мимо туннеля не идёт ничего.")
        }
        NetworksCard(frame, openNetwork)
        GeoCard(frame.geo, c)
        Section("Запуск и защита") {
            Row(
                Modifier.fillMaxWidth().heightIn(min = 60.dp).clickable {
                    context.startActivity(Intent(AndroidSettings.ACTION_VPN_SETTINGS))
                },
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Column(Modifier.weight(1f)) {
                    Text("Постоянный VPN", style = T.body)
                    Text("Android сам держит bliss включённым и может не пускать трафик мимо него. " +
                        "Приложения «мимо VPN» тогда тоже без сети.", style = T.tiny.copy(fontSize = 12.5.sp))
                }
                Ico(Icons.Right, C.N600)
            }
        }
        Row(
            Modifier.padding(bottom = 12.dp).fillMaxWidth().card().clickable(onClick = openTraffic).padding(16.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Ico(Icons.Pulse, C.Accent)
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Text("Трафик, соединения и журнал", style = T.body)
                Text("то же, что по строке скоростей на Главной", style = T.tiny.copy(fontSize = 12.5.sp))
            }
            Ico(Icons.Right, C.N600)
        }
        Row(Modifier.padding(bottom = 24.dp).fillMaxWidth().card().padding(start = 16.dp, top = 16.dp), verticalAlignment = Alignment.Bottom) {
            Column(Modifier.weight(1f).padding(bottom = 18.dp)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Image(painterResource(R.drawable.mark), null, Modifier.size(24.dp))
                    Spacer(Modifier.width(8.dp))
                    Text("bliss ${BuildConfig.VERSION_NAME} · тестовая", style = T.body.copy(fontWeight = FontWeight.Bold, fontSize = 18.sp))
                }
                Text(if (frame.engine.isEmpty()) "движок xray — внутри приложения" else "движок ${frame.engine} — внутри приложения",
                    style = T.tiny.copy(fontSize = 12.5.sp), modifier = Modifier.padding(top = 4.dp))
                Text("Comfortaa · SIL Open Font License", style = T.tiny.copy(fontSize = 12.5.sp))
            }
            Image(painterResource(R.drawable.bunny), "Зайчик спит в чашке", Modifier.size(150.dp, 142.dp))
        }
    }
}

@Composable
private fun Section(title: String, content: @Composable () -> Unit) {
    Column(Modifier.padding(bottom = 12.dp).fillMaxWidth().card().padding(16.dp)) {
        Kicker(title, Modifier.padding(bottom = 8.dp))
        content()
    }
}

@Composable
private fun Line(title: String, hint: String) {
    Column(Modifier.padding(vertical = 4.dp)) {
        Text(title, style = T.body)
        Text(hint, style = T.tiny.copy(fontSize = 12.5.sp))
    }
}

/**
 * The rule sets: which list, what is on the phone, and the button. A
 * category rule (`geosite:youtube`) decides nothing until they are here.
 */
@Composable
private fun GeoCard(geo: GeoView, c: Commands) {
    Section("Гео-файлы") {
        Text("Списки сайтов и адресов по категориям и странам — для правил вида geosite:youtube и geoip:ru.",
            style = T.tiny.copy(fontSize = 12.5.sp), modifier = Modifier.padding(bottom = 10.dp))
        for (source in geo.sources) {
            val on = source.key == geo.source
            Row(
                Modifier.fillMaxWidth().clip(RoundedCornerShape(14.dp))
                    .background(if (on) C.Accent.copy(alpha = 0.08f) else Color.Transparent)
                    .clickable(enabled = geo.fetching == null && !on) { c("geo_update", "source" to source.key) }
                    .padding(horizontal = 10.dp, vertical = 9.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Box(Modifier.size(18.dp).clip(CircleShape).border(1.5.dp, if (on) C.Accent else C.N600, CircleShape),
                    contentAlignment = Alignment.Center) {
                    if (on) Box(Modifier.size(9.dp).clip(CircleShape).background(C.Accent))
                }
                Column(Modifier.weight(1f).padding(start = 12.dp)) {
                    Text(source.name, style = T.body)
                    Text(source.about, style = T.tiny.copy(fontSize = 12.sp))
                }
            }
        }
        val here = geo.ip != null && geo.site != null
        Text(
            when {
                geo.fetching != null -> "Скачиваю… ${bytes(geo.fetching)}"
                geo.problem != null -> "Не вышло: ${geo.problem}"
                here -> "На телефоне: ${bytes((geo.ip ?: 0) + (geo.site ?: 0))}" +
                    (geo.updated?.let { " · обновлены ${ago(it)}" } ?: "")
                else -> "Пока не скачаны"
            },
            style = T.small.copy(color = if (geo.problem != null && geo.fetching == null) C.BadText else C.N400),
            modifier = Modifier.padding(top = 10.dp),
        )
        Btn(
            if (here) "Обновить" else "Скачать", icon = Icons.Refresh, enabled = geo.fetching == null,
            modifier = Modifier.padding(top = 10.dp).fillMaxWidth(),
        ) { c("geo_update", "source" to null) }
        Row(Modifier.padding(top = 12.dp), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("Обновлять раз в неделю", style = T.body)
                Text("когда туннель включается, через него", style = T.tiny.copy(fontSize = 12.5.sp))
            }
            Toggle(geo.auto) { c("geo_auto", "on" to it) }
        }
    }
}

/** In words: what bliss does on a network. */
fun netSummary(connect: String, preset: String?): String {
    val what = when (connect) { "on" -> "подключаюсь сам"; "off" -> "не подключаюсь"; else -> "решает сердечко" }
    return if (preset != null) "$what · пресет «$preset»" else what
}

/**
 * Networks: bliss follows the phone. The one it is on, then «любой Wi-Fi»
 * and mobile data, then every Wi-Fi with a rule of its own. A Wi-Fi is told
 * from another only by its name, which Android gives with location
 * permission alone — asked for here, on a tap, and never at install.
 */
@Composable
private fun NetworksCard(frame: Frame, open: (String, String) -> Unit) {
    val context = LocalContext.current
    var allowed by remember { mutableStateOf(canReadWifiNames(context)) }
    val ask = androidx.activity.compose.rememberLauncherForActivityResult(
        androidx.activity.result.contract.ActivityResultContracts.RequestMultiplePermissions(),
    ) {
        allowed = canReadWifiNames(context)
        if (allowed) TunnelService.reportNetwork(context)
    }
    Section("Сети") {
        Text("Что делать в каждой сети: подключаться самому, не подключаться или оставить сердечку, и какой пресет включить. " +
            "Работает, пока bliss включён или ждёт другой сети.",
            style = T.tiny.copy(fontSize = 12.5.sp), modifier = Modifier.padding(bottom = 6.dp))
        val now = frame.network
        if (now != null) {
            NetLine("Сейчас: ${now.name}", netSummary(now.connect, now.preset) + if (!now.own && now.connect.isNotEmpty()) " (как у любого Wi-Fi)" else "", true) {
                open(now.id, now.name)
            }
        }
        val listed = mutableListOf<Pair<String, String>>()
        listed += "wifi" to "Любой Wi-Fi"
        listed += "mobile" to "Мобильная сеть"
        for (n in frame.networks) if (listed.none { it.first == n.id }) listed += n.id to n.name
        for ((id, name) in listed) {
            if (id == now?.id) continue
            val rule = frame.networks.firstOrNull { it.id == id }
            NetLine(name, netSummary(rule?.connect ?: "", rule?.preset), false) { open(id, name) }
        }
        if (!allowed) {
            Text("Чтобы различать сети Wi-Fi по имени, Android просит доступ к местоположению. " +
                "bliss читает только имя сети и никогда — где телефон.",
                style = T.tiny.copy(fontSize = 12.5.sp), modifier = Modifier.padding(top = 8.dp, bottom = 8.dp))
            Btn("Различать Wi-Fi по имени", accent = false, modifier = Modifier.fillMaxWidth()) {
                ask.launch(arrayOf(android.Manifest.permission.ACCESS_FINE_LOCATION, android.Manifest.permission.ACCESS_COARSE_LOCATION))
            }
        }
    }
}

fun canReadWifiNames(context: android.content.Context) =
    context.checkSelfPermission(android.Manifest.permission.ACCESS_FINE_LOCATION) == android.content.pm.PackageManager.PERMISSION_GRANTED

@Composable
private fun NetLine(title: String, hint: String, current: Boolean, onClick: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().heightIn(min = 56.dp).clip(RoundedCornerShape(14.dp)).clickable(onClick = onClick),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f).padding(vertical = 6.dp)) {
            Text(title, style = T.body.copy(color = if (current) C.Accent else C.Text), maxLines = 1)
            Text(hint, style = T.tiny.copy(fontSize = 12.5.sp), maxLines = 2)
        }
        Ico(Icons.Right, C.N600)
    }
}

/** What bliss does on one network: connect, stay off or leave it to the heart, and a preset. */
@Composable
fun NetworkSheet(frame: Frame, id: String, name: String, c: Commands, close: () -> Unit) {
    val own = frame.networks.firstOrNull { it.id == id }
    val inherited = frame.network?.takeIf { it.id == id }
    var connect by remember { mutableStateOf(own?.connect ?: inherited?.connect ?: "") }
    var preset by remember { mutableStateOf(own?.preset ?: inherited?.preset) }
    fun save() = c("set_network", "id" to id, "connect" to connect, "preset" to preset)
    BlissSheet(close) {
        Column(Modifier.padding(horizontal = 20.dp).padding(top = 6.dp, bottom = 10.dp)) {
            Text(name, style = T.sheet, maxLines = 1)
            Text(if (id == "wifi") "для каждого Wi-Fi без своего правила" else "что делать в этой сети", style = T.small)
        }
        for ((key, label) in listOf("on" to "Подключаться самому", "off" to "Не подключаться", "" to "Как решит сердечко")) {
            MenuRow(if (connect == key) Icons.Check else Icons.Circle,
                label, if (connect == key) C.Accent else C.N300) { connect = key; save() }
        }
        if (frame.presets.isNotEmpty()) {
            Text("Пресет здесь", style = T.small.copy(color = C.N400), modifier = Modifier.padding(start = 20.dp, top = 12.dp, bottom = 8.dp))
            Row(Modifier.padding(horizontal = 16.dp).horizontalScroll(rememberScrollState()),
                horizontalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(8.dp)) {
                Chip("Не менять", preset == null) { preset = null; save() }
                for (p in frame.presets) Chip(p, preset == p) { preset = p; save() }
            }
        }
        Text("Подключаюсь и отключаюсь сам, пока bliss включён или ждёт другой сети; в Wi-Fi, который просит войти, — после входа.",
            style = T.tiny.copy(fontSize = 12.5.sp), modifier = Modifier.padding(start = 20.dp, end = 20.dp, top = 14.dp, bottom = 8.dp))
    }
}
