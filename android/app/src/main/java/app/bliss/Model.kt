package app.bliss

import android.net.LocalSocket
import android.net.LocalSocketAddress
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import org.json.JSONArray
import org.json.JSONObject
import java.io.BufferedReader
import java.io.InputStreamReader
import java.io.OutputStream
import kotlin.concurrent.thread

/** What the engine publishes (crates/android/src/protocol.rs, `Frame`). */
data class Frame(
    val state: String = "off",
    val step: Int = 0,
    val error: String? = null,
    val suggest: Suggest? = null,
    val since: Long? = null,
    val current: Int? = null,
    val servers: List<ServerRow> = emptyList(),
    val groups: List<GroupRow> = emptyList(),
    val rules: List<RuleRow> = emptyList(),
    val fallback: String = "proxy",
    val presets: List<String> = emptyList(),
    val preset: String? = null,
    val drifted: Boolean = false,
    val warnings: List<String> = emptyList(),
    val numbers: Numbers = Numbers(),
    val history: List<Long> = emptyList(),
    val table: List<ConnRow>? = null,
    val log: List<String>? = null,
    val toast: Toast? = null,
    val engine: String = "",
    val canRestore: Boolean = false,
    val ticket: Long = 0,
    val holding: Boolean = false,
    val geo: GeoView = GeoView(),
    /** Every geosite:/geoip: category, only while the rule editor asks. */
    val categories: List<String>? = null,
    /** Not from the engine: whether the socket is up at all. */
    val connected: Boolean = false,
)

data class GeoSource(val key: String, val name: String, val about: String)
data class GeoView(
    val source: String = "", val sources: List<GeoSource> = emptyList(), val auto: Boolean = true,
    val ip: Long? = null, val site: Long? = null, val updated: Long? = null,
    val fetching: Long? = null, val problem: String? = null,
)

data class Suggest(val index: Int, val name: String, val ping: Int)
data class Numbers(
    val downRate: Long = 0, val upRate: Long = 0, val downTotal: Long = 0, val upTotal: Long = 0,
    val active: Int = 0, val opened: Long = 0, val latency: Int? = null,
)
data class ServerRow(
    val name: String, val kind: String, val detail: String, val address: String,
    val group: String?, val ping: String, val ms: Int?,
    /** The real check: a page fetched through the server ("ok", "down", "probing", "unknown"). */
    val check: String = "unknown", val checkMs: Int? = null,
)
data class GroupRow(
    val id: String, val name: String, val host: String, val updated: Long?, val used: Long?,
    val total: Long?, val expire: Long?, val fetching: Boolean, val problem: String?,
)
data class RuleRow(
    val name: String, val conditions: String, val action: String, val enabled: Boolean,
    /** Whether the name was given rather than made up from the conditions. */
    val named: Boolean = false,
    /** The conditions one to a line, as the editor shows them. */
    val text: String = "",
    /** The package, for a rule about one app and nothing else. */
    val app: String? = null,
)
data class ConnRow(
    val target: String, val route: String, val rule: String, val up: Long, val down: Long, val closed: Boolean,
    val app: String? = null,
)
data class Toast(val id: Long, val text: String, val bad: Boolean, val undo: Boolean)

private fun JSONObject.str(key: String): String? = if (isNull(key)) null else optString(key)
private fun JSONObject.long(key: String): Long? = if (isNull(key) || !has(key)) null else optLong(key)
private fun JSONObject.int(key: String): Int? = if (isNull(key) || !has(key)) null else optInt(key)
private inline fun <T> JSONArray?.map(f: (JSONObject) -> T): List<T> =
    if (this == null) emptyList() else (0 until length()).map { f(getJSONObject(it)) }

fun parseFrame(text: String): Frame {
    val o = JSONObject(text)
    val n = o.optJSONObject("numbers") ?: JSONObject()
    return Frame(
        state = o.optString("state", "off"),
        step = o.optInt("step"),
        error = o.str("error"),
        suggest = o.optJSONObject("suggest")?.let { Suggest(it.getInt("index"), it.getString("name"), it.getInt("ping")) },
        since = o.long("since"),
        current = o.int("current"),
        servers = o.optJSONArray("servers").map {
            ServerRow(it.getString("name"), it.getString("kind"), it.getString("detail"), it.getString("address"),
                it.str("group"), it.getString("ping"), it.int("ms"),
                it.optString("check", "unknown"), it.int("check_ms"))
        },
        groups = o.optJSONArray("groups").map {
            GroupRow(it.getString("id"), it.getString("name"), it.getString("host"), it.long("updated"), it.long("used"),
                it.long("total"), it.long("expire"), it.optBoolean("fetching"), it.str("problem"))
        },
        rules = o.optJSONArray("rules").map {
            RuleRow(it.getString("name"), it.getString("conditions"), it.getString("action"), it.optBoolean("enabled"),
                it.optBoolean("named"), it.optString("text"), it.str("app"))
        },
        fallback = o.optString("fallback", "proxy"),
        presets = o.optJSONArray("presets")?.let { a -> (0 until a.length()).map { a.getString(it) } } ?: emptyList(),
        preset = o.str("preset"),
        drifted = o.optBoolean("drifted"),
        warnings = o.optJSONArray("warnings")?.let { a -> (0 until a.length()).map { a.getString(it) } } ?: emptyList(),
        numbers = Numbers(n.optLong("down_rate"), n.optLong("up_rate"), n.optLong("down_total"), n.optLong("up_total"),
            n.optInt("active"), n.optLong("opened"), n.int("latency")),
        history = o.optJSONArray("history")?.let { a -> (0 until a.length()).map { a.getLong(it) } } ?: emptyList(),
        table = if (o.isNull("table")) null else o.optJSONArray("table").map {
            ConnRow(it.getString("target"), it.getString("route"), it.getString("rule"), it.optLong("up"), it.optLong("down"),
                it.optBoolean("closed"), it.str("app"))
        },
        log = if (o.isNull("log")) null else o.optJSONArray("log")?.let { a -> (0 until a.length()).map { a.getString(it) } },
        toast = o.optJSONObject("toast")?.let { Toast(it.getLong("id"), it.getString("text"), it.optBoolean("bad"), it.optBoolean("undo")) },
        engine = o.optString("engine"),
        canRestore = o.optBoolean("can_restore"),
        ticket = o.optLong("ticket"),
        holding = o.optBoolean("holding"),
        geo = o.optJSONObject("geo")?.let { g ->
            GeoView(
                g.optString("source"),
                g.optJSONArray("sources").map { GeoSource(it.getString("key"), it.getString("name"), it.getString("about")) },
                g.optBoolean("auto", true), g.long("ip"), g.long("site"), g.long("updated"), g.long("fetching"), g.str("problem"),
            )
        } ?: GeoView(),
        categories = if (o.isNull("categories") || !o.has("categories")) null
            else o.optJSONArray("categories")?.let { a -> (0 until a.length()).map { a.getString(it) } },
        connected = true,
    )
}

/**
 * One connection to the engine's socket: frames in, commands out. `full` is
 * whether this client draws the numbers — the engine samples rates only while
 * one does. Reconnects by itself while the tunnel's process starts.
 */
class EngineClient(private val path: String, private val full: Boolean) {
    private val _frames = MutableStateFlow(Frame())
    val frames: StateFlow<Frame> = _frames
    @Volatile private var out: OutputStream? = null
    @Volatile private var socket: LocalSocket? = null
    @Volatile private var running = false

    fun start() {
        if (running) return
        running = true
        thread(name = "bliss-socket", isDaemon = true) {
            var wait = 100L
            while (running) {
                try {
                    val s = LocalSocket(LocalSocket.SOCKET_STREAM)
                    s.connect(LocalSocketAddress(path, LocalSocketAddress.Namespace.FILESYSTEM))
                    socket = s
                    out = s.outputStream
                    send(JSONObject().put("cmd", "hello").put("full", full))
                    wait = 100L
                    val reader = BufferedReader(InputStreamReader(s.inputStream, Charsets.UTF_8))
                    while (running) {
                        val line = reader.readLine() ?: break
                        try { _frames.value = parseFrame(line) } catch (_: Exception) { }
                    }
                } catch (_: Exception) {
                }
                out = null
                try { socket?.close() } catch (_: Exception) { }
                socket = null
                _frames.value = _frames.value.copy(connected = false)
                if (!running) break
                Thread.sleep(wait)
                wait = (wait * 2).coerceAtMost(2000)
            }
        }
    }

    fun stop() {
        running = false
        try { socket?.shutdownInput() } catch (_: Exception) { }
        try { socket?.close() } catch (_: Exception) { }
    }

    fun send(command: JSONObject) {
        val stream = out ?: return
        thread(isDaemon = true) {
            try {
                synchronized(this) { stream.write((command.toString() + "\n").toByteArray(Charsets.UTF_8)); stream.flush() }
            } catch (_: Exception) { }
        }
    }

    fun command(name: String, vararg pairs: Pair<String, Any?>) {
        val o = JSONObject().put("cmd", name)
        for ((k, v) in pairs) o.put(k, v ?: JSONObject.NULL)
        send(o)
    }
}
