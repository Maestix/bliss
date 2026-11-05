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
import java.util.concurrent.Executors
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
    /** Counts the journals written for sharing; a new number: journal.txt is ready. */
    val journal: Long = 0,
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
    /** Which server this is when a command names it back (a hash of its link). */
    val key: String,
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

private fun JSONArray?.strings(): List<String> =
    if (this == null) emptyList() else (0 until length()).map { getString(it) }

/**
 * A line from the engine laid over the frame before it. A line holds only
 * the parts that changed since the last one (`Sent` in the engine), so the
 * server list and the rules are parsed — and handed to the screens as new
 * lists — only when they did change, not with every tick of the numbers.
 */
fun applyPatch(prev: Frame, o: JSONObject): Frame {
    var f = prev
    for (key in o.keys()) {
        f = when (key) {
            "state" -> f.copy(state = o.optString(key, "off"))
            "step" -> f.copy(step = o.optInt(key))
            "error" -> f.copy(error = o.str(key))
            "suggest" -> f.copy(suggest = o.optJSONObject(key)?.let { Suggest(it.getInt("index"), it.getString("name"), it.getInt("ping")) })
            "since" -> f.copy(since = o.long(key))
            "current" -> f.copy(current = o.int(key))
            "servers" -> f.copy(servers = o.optJSONArray(key).map {
                ServerRow(it.optString("key"), it.getString("name"), it.getString("kind"), it.getString("detail"), it.getString("address"),
                    it.str("group"), it.getString("ping"), it.int("ms"),
                    it.optString("check", "unknown"), it.int("check_ms"))
            })
            "groups" -> f.copy(groups = o.optJSONArray(key).map {
                GroupRow(it.getString("id"), it.getString("name"), it.getString("host"), it.long("updated"), it.long("used"),
                    it.long("total"), it.long("expire"), it.optBoolean("fetching"), it.str("problem"))
            })
            "rules" -> f.copy(rules = o.optJSONArray(key).map {
                RuleRow(it.getString("name"), it.getString("conditions"), it.getString("action"), it.optBoolean("enabled"),
                    it.optBoolean("named"), it.optString("text"), it.str("app"))
            })
            "fallback" -> f.copy(fallback = o.optString(key, "proxy"))
            "presets" -> f.copy(presets = o.optJSONArray(key).strings())
            "preset" -> f.copy(preset = o.str(key))
            "drifted" -> f.copy(drifted = o.optBoolean(key))
            "warnings" -> f.copy(warnings = o.optJSONArray(key).strings())
            "numbers" -> {
                val n = o.optJSONObject(key) ?: JSONObject()
                f.copy(numbers = Numbers(n.optLong("down_rate"), n.optLong("up_rate"), n.optLong("down_total"), n.optLong("up_total"),
                    n.optInt("active"), n.optLong("opened"), n.int("latency")))
            }
            "history" -> f.copy(history = o.optJSONArray(key)?.let { a -> (0 until a.length()).map { a.getLong(it) } } ?: emptyList())
            "table" -> f.copy(table = if (o.isNull(key)) null else o.optJSONArray(key).map {
                ConnRow(it.getString("target"), it.getString("route"), it.getString("rule"), it.optLong("up"), it.optLong("down"),
                    it.optBoolean("closed"), it.str("app"))
            })
            "log" -> f.copy(log = if (o.isNull(key)) null else o.optJSONArray(key).strings())
            "toast" -> f.copy(toast = o.optJSONObject(key)?.let { Toast(it.getLong("id"), it.getString("text"), it.optBoolean("bad"), it.optBoolean("undo")) })
            "engine" -> f.copy(engine = o.optString(key))
            "can_restore" -> f.copy(canRestore = o.optBoolean(key))
            "ticket" -> f.copy(ticket = o.optLong(key))
            "holding" -> f.copy(holding = o.optBoolean(key))
            "geo" -> f.copy(geo = o.optJSONObject(key)?.let { g ->
                GeoView(
                    g.optString("source"),
                    g.optJSONArray("sources").map { GeoSource(it.getString("key"), it.getString("name"), it.getString("about")) },
                    g.optBoolean("auto", true), g.long("ip"), g.long("site"), g.long("updated"), g.long("fetching"), g.str("problem"),
                )
            } ?: GeoView())
            "journal" -> f.copy(journal = o.optLong(key))
            "categories" -> f.copy(categories = if (o.isNull(key)) null else o.optJSONArray(key).strings())
            else -> f
        }
    }
    return if (f.connected) f else f.copy(connected = true)
}

/**
 * One connection to the engine's socket: frames in, commands out. `full` is
 * whether this client draws the numbers — the engine samples rates only while
 * one does. Reconnects by itself while the tunnel's process starts.
 */
class EngineClient(private val path: String, private val full: Boolean) {
    private val _frames = MutableStateFlow(Frame())
    val frames: StateFlow<Frame> = _frames
    @Volatile private var socket: LocalSocket? = null
    @Volatile private var running = false
    /** Which start the reading thread belongs to: a stop and a start in quick
     *  succession (onStop, onStart) must not leave two threads, the old one
     *  clearing the new one's socket. */
    @Volatile private var generation = 0

    /** Every write goes through this one thread, in the order the screens
     *  sent them: a thread per command let «log off» from one tab's cleanup
     *  land after «log on» from the next tab's start, and the journal stayed
     *  empty. */
    private val writer = Executors.newSingleThreadExecutor { r -> Thread(r, "bliss-socket-out").apply { isDaemon = true } }
    /** Touched on the writer's thread only. */
    private var out: OutputStream? = null
    /** Commands sent while there is no connection — the tunnel's process
     *  starting — written once there is one, rather than dropped. */
    private val waiting = ArrayDeque<String>()

    fun start() {
        if (running) return
        running = true
        val mine = ++generation
        thread(name = "bliss-socket", isDaemon = true) {
            var wait = 100L
            while (running && generation == mine) {
                var s: LocalSocket? = null
                try {
                    s = LocalSocket(LocalSocket.SOCKET_STREAM)
                    s.connect(LocalSocketAddress(path, LocalSocketAddress.Namespace.FILESYSTEM))
                    socket = s
                    val stream = s.outputStream
                    val hello = JSONObject().put("cmd", "hello").put("full", full).toString()
                    writer.execute {
                        out = stream
                        write(hello)
                        while (out != null && waiting.isNotEmpty()) write(waiting.removeFirst())
                    }
                    wait = 100L
                    val reader = BufferedReader(InputStreamReader(s.inputStream, Charsets.UTF_8))
                    // A new connection starts from nothing: the engine sends
                    // every part first, then what changes.
                    var frame = Frame()
                    while (running && generation == mine) {
                        val line = reader.readLine() ?: break
                        try {
                            frame = applyPatch(frame, JSONObject(line))
                            _frames.value = frame
                        } catch (_: Exception) { }
                    }
                } catch (_: Exception) {
                }
                val stream = try { s?.outputStream } catch (_: Exception) { null }
                writer.execute { if (out === stream) out = null }
                try { s?.close() } catch (_: Exception) { }
                if (socket === s) socket = null
                if (generation == mine) _frames.value = _frames.value.copy(connected = false)
                if (!running || generation != mine) break
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

    /** On the writer's thread: one line, or the connection is taken as gone. */
    private fun write(line: String) {
        val stream = out ?: run { hold(line); return }
        try {
            stream.write((line + "\n").toByteArray(Charsets.UTF_8))
            stream.flush()
        } catch (_: Exception) {
            out = null
            hold(line)
        }
    }

    private fun hold(line: String) {
        // A few, not without end: an engine that never comes back must not
        // grow this. The oldest go first.
        if (waiting.size >= 32) waiting.removeFirst()
        waiting.addLast(line)
    }

    fun send(command: JSONObject) {
        val line = command.toString()
        writer.execute { write(line) }
    }

    fun command(name: String, vararg pairs: Pair<String, Any?>) {
        val o = JSONObject().put("cmd", name)
        for ((k, v) in pairs) o.put(k, v ?: JSONObject.NULL)
        send(o)
    }
}
