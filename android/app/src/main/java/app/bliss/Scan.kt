package app.bliss

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.ImageDecoder
import android.net.Uri
import android.provider.Settings
import android.util.Size
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.Camera
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.ImageProxy
import androidx.camera.core.Preview
import androidx.camera.core.resolutionselector.ResolutionSelector
import androidx.camera.core.resolutionselector.ResolutionStrategy
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size as Box2
import androidx.compose.ui.graphics.BlendMode
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.CompositingStrategy
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.compose.LocalLifecycleOwner
import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.LuminanceSource
import com.google.zxing.MultiFormatReader
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.RGBLuminanceSource
import com.google.zxing.ReaderException
import com.google.zxing.common.HybridBinarizer
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.util.concurrent.Executors

/**
 * QR codes, read by ZXing's core: no Google Play services, nothing sent
 * anywhere. Light-on-dark codes are read too (`ALSO_INVERTED`), since a
 * dark-themed messenger draws them that way.
 */
private object Qr {
    private fun hints(hard: Boolean): Map<DecodeHintType, Any> = buildMap {
        put(DecodeHintType.POSSIBLE_FORMATS, listOf(BarcodeFormat.QR_CODE))
        put(DecodeHintType.ALSO_INVERTED, true)
        put(DecodeHintType.CHARACTER_SET, "UTF-8")
        // Every camera frame gets the quick look; a picture is read once
        // and gets the thorough one.
        if (hard) put(DecodeHintType.TRY_HARDER, true)
    }

    fun read(source: LuminanceSource, hard: Boolean): String? {
        val reader = MultiFormatReader()
        return try {
            reader.decode(BinaryBitmap(HybridBinarizer(source)), hints(hard)).text
        } catch (_: ReaderException) {
            null
        } catch (_: RuntimeException) {
            // ZXing throws its own and, on odd input, Java's; neither is ours to crash on.
            null
        }
    }

    /** A camera frame: its brightness plane is all a QR code needs. */
    fun read(image: ImageProxy): String? {
        val plane = image.planes[0]
        val buffer = plane.buffer.duplicate().apply { rewind() }
        val stride = plane.rowStride
        // The last row may stop at the image's width rather than the stride.
        val data = ByteArray(stride * image.height)
        buffer.get(data, 0, minOf(buffer.remaining(), data.size))
        val source = PlanarYUVLuminanceSource(data, stride, image.height, 0, 0, image.width, image.height, false)
        return read(source, hard = false)
    }

    /** The longest side a picture is read at: larger only costs time. */
    private const val SIDE = 2048

    /** A picture from the gallery: a screenshot of a code, most often. */
    fun read(context: Context, uri: Uri): String? {
        val bitmap = try {
            ImageDecoder.decodeBitmap(ImageDecoder.createSource(context.contentResolver, uri)) { decoder, info, _ ->
                decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
                val long = maxOf(info.size.width, info.size.height)
                if (long > SIDE) {
                    decoder.setTargetSize(info.size.width * SIDE / long, info.size.height * SIDE / long)
                }
            }
        } catch (_: Exception) {
            return null
        }
        // A large code in a large picture reads better smaller; try both.
        return read(bitmap) ?: read(Bitmap.createScaledBitmap(bitmap, bitmap.width / 2, bitmap.height / 2, true))
    }

    private fun read(bitmap: Bitmap): String? {
        if (bitmap.width < 21 || bitmap.height < 21) return null
        val pixels = IntArray(bitmap.width * bitmap.height)
        bitmap.copy(Bitmap.Config.ARGB_8888, false).getPixels(pixels, 0, bitmap.width, 0, 0, bitmap.width, bitmap.height)
        return read(RGBLuminanceSource(bitmap.width, bitmap.height, pixels), hard = true)
    }
}

/**
 * The QR scanner: the camera, or a picture from the gallery. A code that
 * reads as a server or a subscription is imported at once, as «Вставить»
 * does; anything else is said by kind, never quoted, and scanning goes on.
 */
@Composable
fun Scanner(back: () -> Unit, found: (String) -> Unit) {
    val context = LocalContext.current
    val haptic = LocalHapticFeedback.current
    val scope = rememberCoroutineScope()
    var allowed by remember { mutableStateOf(hasCamera(context)) }
    var asked by remember { mutableStateOf(false) }
    var note by remember { mutableStateOf<Pair<String, Color>?>(null) }
    var torch by remember { mutableStateOf(false) }
    var flash by remember { mutableStateOf(false) }
    var done by remember { mutableStateOf(false) }
    // The same wrong code is seen in every frame: said once.
    var refused by remember { mutableStateOf<String?>(null) }

    val take: (String) -> Unit = take@{ text ->
        if (done || text == refused) return@take
        val (said, color, ok) = verdict(text)
        if (ok) {
            done = true
            haptic.performHapticFeedback(HapticFeedbackType.LongPress)
            found(text.trim())
        } else {
            refused = text
            note = (if (text.isBlank()) "В коде пусто" else said) to color
        }
    }

    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {
        allowed = it
        asked = true
    }
    LaunchedEffect(Unit) { if (!allowed) permission.launch(Manifest.permission.CAMERA) }
    // Back from Android's settings with the camera allowed there.
    LifecycleEventEffect(Lifecycle.Event.ON_RESUME) { allowed = hasCamera(context) }

    val gallery = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        note = "Читаю картинку…" to C.N400
        scope.launch {
            val text = withContext(Dispatchers.Default) { Qr.read(context, uri) }
            if (text == null) note = "На картинке не нашлось QR-кода" to C.BadText
            else { refused = null; take(text) }
        }
    }

    Box(Modifier.fillMaxSize().background(Color.Black)) {
        if (allowed) {
            CameraPreview(torch, onFlash = { flash = it }, onFail = { note = "Камера не открылась — выберите картинку" to C.BadText },
                onText = take, modifier = Modifier.fillMaxSize())
            ScanWindow(Modifier.fillMaxSize())
        } else if (asked) {
            Column(
                Modifier.align(Alignment.Center).padding(horizontal = 32.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(14.dp),
            ) {
                Ico(Icons.Qr, C.N400, 48.dp)
                Text("Без камеры QR не прочитать, но можно выбрать картинку с кодом — например, скриншот.",
                    style = T.body.copy(color = C.N300, textAlign = TextAlign.Center))
                Btn("Разрешить камеру", accent = false) {
                    context.startActivity(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", context.packageName, null)))
                }
            }
        }

        Row(Modifier.statusBarsPadding().padding(horizontal = 12.dp).height(56.dp), verticalAlignment = Alignment.CenterVertically) {
            IconBtn(Icons.Back, C.Text, "Назад", back)
            Column {
                Text("QR-код", style = T.title.copy(fontSize = 22.sp))
                Text("наведите на код со ссылкой на сервер или подписку", style = T.tiny.copy(fontSize = 12.5.sp, color = C.N300))
            }
        }

        Column(
            Modifier.align(Alignment.BottomCenter).fillMaxWidth().navigationBarsPadding().padding(16.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            note?.let { (text, color) ->
                Text(text, style = T.small.copy(color = color, textAlign = TextAlign.Center),
                    modifier = Modifier.padding(bottom = 14.dp).fillMaxWidth()
                        .background(Color(0xCC0A0610), androidx.compose.foundation.shape.RoundedCornerShape(14.dp))
                        .padding(horizontal = 14.dp, vertical = 10.dp))
            }
            Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                Btn("Из галереи", accent = false, icon = Icons.Picture, modifier = Modifier.weight(1f)) {
                    gallery.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly))
                }
                if (allowed && flash) {
                    Btn(if (torch) "Погасить" else "Фонарик", accent = torch, icon = Icons.Torch, modifier = Modifier.weight(1f)) { torch = !torch }
                }
            }
        }
    }
}

private fun hasCamera(context: Context) =
    ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED

/** The back camera, frames handed to ZXing on a thread of their own. */
@Composable
private fun CameraPreview(torch: Boolean, onFlash: (Boolean) -> Unit, onFail: () -> Unit, onText: (String) -> Unit, modifier: Modifier) {
    val context = LocalContext.current
    val owner = LocalLifecycleOwner.current
    val latest by rememberUpdatedState(onText)
    val view = remember {
        PreviewView(context).apply {
            implementationMode = PreviewView.ImplementationMode.COMPATIBLE
            scaleType = PreviewView.ScaleType.FILL_CENTER
        }
    }
    var camera by remember { mutableStateOf<Camera?>(null) }

    DisposableEffect(owner) {
        val worker = Executors.newSingleThreadExecutor()
        val main = ContextCompat.getMainExecutor(context)
        val future = ProcessCameraProvider.getInstance(context)
        var provider: ProcessCameraProvider? = null
        var gone = false
        future.addListener({
            if (gone) return@addListener
            try {
                val p = future.get().also { provider = it }
                val preview = Preview.Builder().build().also { it.setSurfaceProvider(view.surfaceProvider) }
                val analysis = ImageAnalysis.Builder()
                    // Enough for a code across a third of the frame, and cheap to read.
                    .setResolutionSelector(ResolutionSelector.Builder().setResolutionStrategy(
                        ResolutionStrategy(Size(1280, 720), ResolutionStrategy.FALLBACK_RULE_CLOSEST_HIGHER_THEN_LOWER)).build())
                    .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                    .build()
                analysis.setAnalyzer(worker) { image ->
                    val text = image.use { Qr.read(it) }
                    if (text != null) main.execute { if (!gone) latest(text) }
                }
                val lens = when {
                    p.hasCamera(CameraSelector.DEFAULT_BACK_CAMERA) -> CameraSelector.DEFAULT_BACK_CAMERA
                    else -> CameraSelector.DEFAULT_FRONT_CAMERA
                }
                p.unbindAll()
                camera = p.bindToLifecycle(owner, lens, preview, analysis).also { onFlash(it.cameraInfo.hasFlashUnit()) }
            } catch (_: Exception) {
                onFail()
            }
        }, main)
        onDispose {
            gone = true
            camera = null
            provider?.unbindAll()
            worker.shutdown()
        }
    }
    LaunchedEffect(camera, torch) { camera?.cameraControl?.enableTorch(torch) }
    AndroidView({ view }, modifier)
}

/** The dimmed screen around a window for the code, with its corners in pink. */
@Composable
private fun ScanWindow(modifier: Modifier) {
    Box(
        modifier
            .graphicsLayer { compositingStrategy = CompositingStrategy.Offscreen }
            .drawWithContent {
                val side = minOf(size.width, size.height) * 0.68f
                val at = Offset((size.width - side) / 2, (size.height - side) / 2 - size.height * 0.04f)
                val round = CornerRadius(28.dp.toPx())
                drawRect(Color(0x990A0610))
                drawRoundRect(Color.Transparent, at, Box2(side, side), round, blendMode = BlendMode.Clear)
                drawRoundRect(C.Accent, at, Box2(side, side), round, style = Stroke(2.5.dp.toPx()))
            },
    )
}
