package app.bliss

import android.content.Intent
import android.provider.Settings as AndroidSettings
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

@Composable
fun Settings(frame: Frame, openTraffic: () -> Unit) {
    val context = LocalContext.current
    Column(
        Modifier.fillMaxSize().statusBarsPadding().verticalScroll(rememberScrollState()).padding(horizontal = 16.dp),
    ) {
        Text("Настройки", style = T.title, modifier = Modifier.padding(top = 8.dp, bottom = 12.dp))

        Section("Что идёт через bliss") {
            Line("Весь телефон", "VPN-сервис Android: каждое приложение, UDP, звонки и игры. " +
                "IPv6 пока отклоняется, и приложения уходят на IPv4 — мимо туннеля не идёт ничего.")
        }
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
                    Text("bliss 0.1 · тестовая", style = T.body.copy(fontWeight = FontWeight.Bold, fontSize = 18.sp))
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
