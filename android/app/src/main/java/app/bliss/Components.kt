package app.bliss

import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.spring
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.scale
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.PathParser
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

// --- icons: the desktop's set (crates/app/src/icons.rs), one stroke width ---

private fun circle(cx: Float, cy: Float, r: Float) =
    "M${cx - r} $cy a$r $r 0 1 0 ${2 * r} 0 a$r $r 0 1 0 ${-2 * r} 0"

private fun icon(name: String, vararg paths: String, stroke: Float = 1.6f, fill: Boolean = false): ImageVector {
    val builder = ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f)
    for (d in paths) {
        builder.addPath(
            pathData = PathParser().parsePathString(d).toNodes(),
            fill = if (fill) SolidColor(Color.Black) else null,
            stroke = SolidColor(Color.Black),
            strokeLineWidth = stroke,
            strokeLineCap = StrokeCap.Round,
            strokeLineJoin = StrokeJoin.Round,
        )
    }
    return builder.build()
}

object Icons {
    private const val HEART = "M12 20.4 C10.4 19 4.2 14.9 4.2 10.4 A4.4 4.4 0 0 1 12 7.8 A4.4 4.4 0 0 1 19.8 10.4 C19.8 14.9 13.6 19 12 20.4 Z"
    val Heart = icon("heart", HEART, stroke = 1.7f)
    val HeartFilled = icon("heartFilled", HEART, stroke = 1.7f, fill = true)
    val Globe = icon("globe", circle(12f, 12f, 8.6f), "M3.4 12 H20.6", "M12 3.4 a13 13 0 0 1 0 17.2 a13 13 0 0 1 0-17.2")
    val Split = icon("split", "M4 19 C10 19 8 5 20 5", "M4 5 C10 5 8 19 20 19", "M16.6 2.2 20 5 16.6 7.8", "M16.6 16.2 20 19 16.6 21.8")
    val Gear = icon("gear", circle(12f, 12f, 3.2f), circle(12f, 12f, 8.4f), "M12 3.6 V6.2 M12 17.8 V20.4 M3.6 12 H6.2 M17.8 12 H20.4")
    val Pulse = icon("pulse", "M2.8 12 H7.2 L9.6 5.6 14.4 18.4 16.8 12 H21.2")
    val Plus = icon("plus", "M12 5.4 V18.6 M5.4 12 H18.6", stroke = 1.8f)
    val Up = icon("up", "M6.6 14.8 12 9.4 17.4 14.8")
    val Down = icon("down", "M6.6 9.2 12 14.6 17.4 9.2")
    val Right = icon("right", "M9.2 6.6 14.6 12 9.2 17.4")
    val Back = icon("back", "M14.8 6.6 9.4 12 14.8 17.4", stroke = 1.7f)
    val Check = icon("check", "M5.4 12.6 9.8 17 18.6 7.6", stroke = 1.8f)
    val Trash = icon("trash", "M4.6 6.8 H19.4", "M9.4 6.8 V4.8 a1 1 0 0 1 1-1 H13.6 a1 1 0 0 1 1 1 V6.8", "M6.4 6.8 7.4 19 a1.6 1.6 0 0 0 1.6 1.4 H15 a1.6 1.6 0 0 0 1.6-1.4 L17.6 6.8")
    val Refresh = icon("refresh", "M20 12 A8 8 0 1 1 17.4 6.1", "M20.4 3.4 V7.4 H16.4")
    val Rss = icon("rss", "M5 18.6 h.01", "M5 12.4 a6.2 6.2 0 0 1 6.2 6.2", "M5 6.2 a12.4 12.4 0 0 1 12.4 12.4", stroke = 1.8f)
    val Clip = icon("clip", "M7.4 4.6 H16.6 a2.4 2.4 0 0 1 2.4 2.4 V18.6 a2.4 2.4 0 0 1-2.4 2.4 H7.4 a2.4 2.4 0 0 1-2.4-2.4 V7 a2.4 2.4 0 0 1 2.4-2.4 Z", "M9 4.6 V3.4 H15 V4.6", "M8.6 10.4 H15.4 M8.6 14 H13.2")
    val More = icon("more", circle(6f, 12f, 0.6f), circle(12f, 12f, 0.6f), circle(18f, 12f, 0.6f), stroke = 1.8f)
    val Close = icon("close", "M6 6 L18 18 M18 6 L6 18")
    val Qr = icon("qr", "M4.4 4.4 H9.6 V9.6 H4.4 Z", "M14.4 4.4 H19.6 V9.6 H14.4 Z", "M4.4 14.4 H9.6 V19.6 H4.4 Z",
        "M14.4 14.4 H16.4 M19.6 14.4 V16.6 M14.4 17.6 V19.6 H19.6 M17.2 17.2 h.01", stroke = 1.6f)
    val Picture = icon("picture", "M5.6 4.6 H18.4 a2 2 0 0 1 2 2 V17.4 a2 2 0 0 1-2 2 H5.6 a2 2 0 0 1-2-2 V6.6 a2 2 0 0 1 2-2 Z",
        circle(9f, 9.4f, 1.6f), "M3.8 16.4 8.6 12.4 12.4 15.6 15.4 13 20.2 17")
    val Torch = icon("torch", "M13.2 2.8 6.4 13.4 H11.6 L10.4 21.2 17.6 10.4 H12.4 Z", stroke = 1.6f)
    val Circle = icon("circle", circle(12f, 12f, 6.4f), stroke = 1.5f)
    val Share = icon("share", circle(17.6f, 5.6f, 2.4f), circle(6.4f, 12f, 2.4f), circle(17.6f, 18.4f, 2.4f),
        "M8.5 10.8 15.5 6.8 M8.5 13.2 15.5 17.2", stroke = 1.6f)
    val Shield = icon("shield","M12 2.6 20 5.6 V12 c0 5-3.6 8-8 9.4 C8 20 4.4 17 4.4 12 V5.6 Z", "M8.6 11.8 11.2 14.4 15.6 9.6", stroke = 1.7f)
}

@Composable
fun Ico(icon: ImageVector, tint: Color, size: Dp = 22.dp, modifier: Modifier = Modifier) =
    Icon(icon, contentDescription = null, tint = tint, modifier = modifier.size(size))

// --- surfaces ----------------------------------------------------------------

fun Modifier.card(): Modifier = this
    .clip(S.card)
    .background(C.Surface)
    .border(1.dp, Color(0x0FEFE8F5), S.card)

fun Modifier.tile(): Modifier = this.clip(S.tile).background(C.Tint)

// --- the ring: the one switch for the tunnel ----------------------------------

private fun tone(state: String) = when (state) {
    "on" -> C.Accent
    "busy" -> C.Violet
    "failed" -> C.Bad
    else -> C.N600
}

@Composable
fun Ring(state: String, size: Dp, onClick: () -> Unit) {
    val color by animateColorAsState(tone(state), tween(300), label = "ring")
    val haptics = LocalHapticFeedback.current
    val pulse = rememberInfiniteTransition(label = "pulse")
    val beat by pulse.animateFloat(
        1f, if (state == "busy") 1.035f else 1f,
        infiniteRepeatable(tween(700), RepeatMode.Reverse), label = "beat",
    )
    val glow = if (state == "off") 0.16f else 0.34f
    Box(
        Modifier
            .size(size)
            .scale(beat)
            .drawBehind {
                // The glow is a gradient drawn once, not a blur.
                drawCircle(
                    Brush.radialGradient(
                        listOf(color.copy(alpha = glow), Color.Transparent),
                        center = Offset(this.size.width / 2, this.size.height / 2),
                        radius = this.size.minDimension * 0.85f,
                    ),
                    radius = this.size.minDimension * 0.85f,
                )
                drawCircle(Color(0x73140F1D))
            }
            .border(1.5.dp, color, CircleShape)
            .clip(CircleShape)
            .clickable(role = Role.Button, onClickLabel = if (state == "on" || state == "busy") "Отключить" else "Подключить") {
                haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                onClick()
            },
        contentAlignment = Alignment.Center,
    ) {
        Ico(if (state == "on") Icons.HeartFilled else Icons.Heart, color, size = size * 0.35f)
    }
}

// --- small controls --------------------------------------------------------------

@Composable
fun Chip(text: String, on: Boolean, color: Color = C.Accent, dot: Boolean = false, onClick: () -> Unit) {
    val shape = RoundedCornerShape(20.dp)
    Row(
        Modifier
            .height(40.dp)
            .clip(shape)
            .background(if (on) color.copy(alpha = 0.14f) else Color.Transparent)
            .border(1.dp, if (on) color.copy(alpha = 0.32f) else Color(0x1FEFE8F5), shape)
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Text(text, style = T.body.copy(fontSize = 14.sp, color = if (on) color else C.N400))
        if (dot) Box(Modifier.size(6.dp).clip(CircleShape).background(C.Violet))
    }
}

@Composable
fun Pill(action: String, closed: Boolean = false) {
    val color = if (closed) C.N500 else actionColor(action)
    Row(
        Modifier.height(26.dp).clip(RoundedCornerShape(13.dp)).background(color.copy(alpha = 0.13f)).padding(horizontal = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Box(Modifier.size(7.dp).clip(CircleShape).background(color))
        Text(if (closed) "закрыто" else actionWord(action), style = T.tiny.copy(color = color, fontSize = 12.5.sp))
    }
}

/** The desktop's rule switch at finger size, and as quiet. */
@Composable
fun Toggle(on: Boolean, color: Color = C.Accent, onChange: (Boolean) -> Unit) {
    val knob by androidx.compose.animation.core.animateDpAsState(if (on) 25.dp else 5.dp, spring(), label = "knob")
    val shape = RoundedCornerShape(14.dp)
    Box(
        Modifier
            .size(48.dp, 28.dp)
            .clip(shape)
            .background(if (on) color.copy(alpha = 0.12f) else Color(0x0DEFE8F5))
            .border(1.dp, if (on) color.copy(alpha = 0.34f) else Color(0x29EFE8F5), shape)
            .clickable(role = Role.Switch) { onChange(!on) },
    ) {
        Box(
            Modifier.padding(start = knob, top = 5.dp).size(18.dp).clip(CircleShape)
                .background(if (on) color.copy(alpha = 0.75f) else Color(0x4DEFE8F5))
        )
    }
}

@Composable
fun Btn(text: String, accent: Boolean = true, modifier: Modifier = Modifier, enabled: Boolean = true, icon: ImageVector? = null, onClick: () -> Unit) {
    val shape = RoundedCornerShape(24.dp)
    val color = if (accent) C.Accent else C.N300
    Row(
        modifier
            .height(48.dp)
            .clip(shape)
            .background(if (accent) C.Accent.copy(alpha = 0.12f) else C.Tint)
            .border(1.dp, if (accent) C.Accent.copy(alpha = 0.55f) else Color(0x1AEFE8F5), shape)
            .clickable(enabled = enabled, onClick = onClick)
            .padding(horizontal = 20.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.Center,
    ) {
        if (icon != null) {
            Ico(icon, color.copy(alpha = if (enabled) 1f else 0.45f), 18.dp)
            Box(Modifier.width(8.dp))
        }
        Text(text, style = T.body.copy(fontWeight = androidx.compose.ui.text.font.FontWeight.Bold, color = color.copy(alpha = if (enabled) 1f else 0.45f)))
    }
}

@Composable
fun IconBtn(icon: ImageVector, tint: Color = C.N400, label: String, onClick: () -> Unit) {
    Box(
        Modifier.size(44.dp).clip(CircleShape).clickable(onClickLabel = label, onClick = onClick),
        contentAlignment = Alignment.Center,
    ) { Ico(icon, tint, 22.dp) }
}

@Composable
fun Kicker(text: String, modifier: Modifier = Modifier) =
    Text(text.uppercase(), style = T.kicker, modifier = modifier)

@Composable
fun RowScope.Grow() = Box(Modifier.weight(1f))

/** Ping: three bands, not a gradient (the desktop's `ping_tone`). */
fun pingColor(row: ServerRow): Color = when (row.ping) {
    "ok" -> when {
        (row.ms ?: 0) < 120 -> C.Good
        (row.ms ?: 0) < 300 -> C.Violet300
        else -> C.N400
    }
    "down" -> C.BlockText
    else -> C.N500
}

fun pingText(row: ServerRow): String = when (row.ping) {
    "ok" -> "${row.ms} мс"
    "down" -> "нет"
    "udp" -> "UDP"
    "probing" -> "…"
    else -> "—"
}

/**
 * Whether a server takes you somewhere: a page fetched through it, timed.
 * Under half a second is good, under one and a half usable.
 */
fun checkColor(row: ServerRow): Color = when (row.check) {
    "ok" -> when {
        (row.checkMs ?: 0) < 500 -> C.Good
        (row.checkMs ?: 0) < 1500 -> C.Violet300
        else -> C.N400
    }
    "down" -> C.BlockText
    else -> C.N500
}

fun checkText(row: ServerRow): String = when (row.check) {
    "ok" -> "${row.checkMs} мс"
    "down" -> "нет доступа"
    "probing" -> "проверка…"
    else -> ""
}

/** The download rate over the last seconds: a line and a soft fill; flat and dim while off. */
@Composable
fun RateChart(history: List<Long>, active: Boolean, modifier: Modifier) {
    Canvas(modifier) {
        val tone = if (active) C.Accent else C.N700
        if (history.size < 2 || !active) {
            drawLine(tone, Offset(0f, size.height - 1f), Offset(size.width, size.height - 1f), strokeWidth = 1.5.dp.toPx())
            return@Canvas
        }
        val top = (history.maxOrNull() ?: 1L).coerceAtLeast(1L).toFloat()
        val step = size.width / (history.size - 1)
        val points = history.mapIndexed { i, v -> Offset(i * step, size.height - v / top * size.height * 0.9f - 1f) }
        val line = Path().apply {
            moveTo(points[0].x, points[0].y)
            for (i in 1 until points.size) {
                val a = points[i - 1]; val b = points[i]
                // Control points at the samples' own heights: the curve never
                // overshoots, so it never dips below zero beside a burst.
                val mid = (a.x + b.x) / 2
                cubicTo(mid, a.y, mid, b.y, b.x, b.y)
            }
        }
        val fill = Path().apply { addPath(line); lineTo(size.width, size.height); lineTo(0f, size.height); close() }
        drawPath(fill, Brush.verticalGradient(listOf(tone.copy(alpha = 0.30f), tone.copy(alpha = 0.02f))))
        drawPath(line, tone, style = Stroke(width = 1.8.dp.toPx()))
    }
}

fun bytes(n: Long): String = when {
    n < 1024 -> "$n Б"
    n < 1024 * 1024 -> "%.0f КБ".format(n / 1024.0)
    n < 1024L * 1024 * 1024 -> "%.1f МБ".format(n / 1048576.0).replace('.', ',')
    else -> "%.2f ГБ".format(n / 1073741824.0).replace('.', ',')
}

fun rate(n: Long): String = bytes(n) + "/с"

fun plural(n: Long, one: String, few: String, many: String): String {
    val word = when {
        n % 100 in 11..14 -> many
        n % 10 == 1L -> one
        n % 10 in 2..4 -> few
        else -> many
    }
    return "$n $word"
}
