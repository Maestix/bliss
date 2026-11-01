package app.bliss

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * The desktop's look at touch size (docs/android-design.md): the colours come
 * from theme.css through the generated `Tokens`; the radii and the sizes are
 * the phone's own.
 */
object C {
    val Bg = Tokens.BG
    val Surface = Tokens.SURFACE
    val Text = Tokens.TEXT
    val Accent = Tokens.ACCENT
    val Accent300 = Tokens.ACCENT_300
    val Violet = Tokens.ACCENT_2
    val Violet300 = Tokens.ACCENT_2_300
    val Good = Tokens.GOOD
    val Bad = Tokens.BAD
    val Vpn = Tokens.VPN
    val N200 = Tokens.NEUTRAL_200
    val N300 = Tokens.NEUTRAL_300
    val N400 = Tokens.NEUTRAL_400
    val N500 = Tokens.NEUTRAL_500
    val N600 = Tokens.NEUTRAL_600
    val N700 = Tokens.NEUTRAL_700

    // Mixes the desktop writes as rgba() in ui.rs and app.css.
    val Raised = Color(0xFF251C31)
    val Sunk = Color(0xFF120C1A)
    val Snack = Color(0xFF2A2036)
    val BadText = Color(0xFFF2A7AD)
    val BlockText = Color(0xFFEC8A8D)
    val Hair = Color(0x1AEFE8F5)
    val Tint = Color(0x0AEFE8F5)
}

val Comfortaa = FontFamily(
    Font(R.font.comfortaa_semibold, FontWeight.SemiBold),
    Font(R.font.comfortaa_bold, FontWeight.Bold),
)

object T {
    val body = TextStyle(fontFamily = Comfortaa, fontWeight = FontWeight.SemiBold, fontSize = 15.sp, color = C.Text)
    val small = body.copy(fontSize = 13.sp, color = C.N500)
    val tiny = body.copy(fontSize = 12.sp, color = C.N500)
    val kicker = body.copy(fontSize = 11.5.sp, color = C.N500, letterSpacing = 1.1.sp)
    val title = body.copy(fontWeight = FontWeight.Bold, fontSize = 26.sp)
    val sheet = body.copy(fontWeight = FontWeight.Bold, fontSize = 20.sp)
    val card = body.copy(fontWeight = FontWeight.Bold, fontSize = 16.sp)
    val state = body.copy(fontWeight = FontWeight.Bold, fontSize = 30.sp)
}

object S {
    val card = RoundedCornerShape(20.dp)
    val tile = RoundedCornerShape(14.dp)
    val pill = RoundedCornerShape(50)
    val sheet = RoundedCornerShape(topStart = 28.dp, topEnd = 28.dp)
}

/** Colour by action, as the desktop's pills: pink proxy, mint direct, red block, blue VPN. */
fun actionColor(action: String): Color = when (action) {
    "direct" -> C.Good
    "block" -> C.BlockText
    "vpn" -> C.Vpn
    else -> C.Accent
}

fun actionWord(action: String): String = when (action) {
    "direct" -> "напрямую"
    "block" -> "блок"
    "vpn" -> "VPN"
    else -> "прокси"
}

@Composable
fun BlissTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = darkColorScheme(
            background = C.Bg, surface = C.Surface, primary = C.Accent, onSurface = C.Text,
            onBackground = C.Text, surfaceContainerLow = C.Surface,
        ),
        content = content,
    )
}
