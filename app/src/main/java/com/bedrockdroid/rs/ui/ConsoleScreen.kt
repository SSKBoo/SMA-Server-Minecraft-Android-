package com.bedrockdroid.rs.ui

import android.widget.Toast
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Fullscreen
import androidx.compose.material.icons.filled.FullscreenExit
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Share
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.bedrockdroid.rs.model.AndroidLauncherConfig
import com.bedrockdroid.rs.model.ConsoleLogLine
import com.bedrockdroid.rs.model.EngineLane
import com.bedrockdroid.rs.model.HardwareScanReport
import com.bedrockdroid.rs.model.LogBuffer
import com.bedrockdroid.rs.model.LogLevel
import com.bedrockdroid.rs.model.LogStream
import com.bedrockdroid.rs.model.NetworkProbeReport

private enum class ConsoleLogFilter(val label: String) {
    ALL("ALL"),
    DEBUG("DEBUG"),
    INFO("INFO"),
    WARN("WARN"),
    ERROR("ERROR")
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
fun ConsoleScreen(
    config: AndroidLauncherConfig,
    isRunning: Boolean,
    logs: List<ConsoleLogLine>,
    hardwareReport: HardwareScanReport?,
    networkReport: NetworkProbeReport?,
    onSelectLane: (EngineLane) -> Unit,
    onStartOrStop: () -> Unit,
    onRunCommand: (String) -> Unit,
    onClearLogs: () -> Unit,
    latestCrashFileName: String? = null,
    latestCrashText: String? = null,
    onShareCrash: (() -> Unit)? = null,
    onDismissCrash: (() -> Unit)? = null,
    modifier: Modifier = Modifier
) {
    val context = LocalContext.current
    val clipboardManager = LocalClipboardManager.current

    var commandText by remember { mutableStateOf("") }
    var activeFilter by remember { mutableStateOf(ConsoleLogFilter.ALL) }
    var searchQuery by remember { mutableStateOf("") }
    var autoScroll by remember { mutableStateOf(true) }
    var isMaximized by remember { mutableStateOf(false) }

    val scrollState = rememberScrollState()

    val filteredLogs = remember(logs, activeFilter, searchQuery) {
        logs.filter { line ->
            val levelMatches = when (activeFilter) {
                ConsoleLogFilter.ALL -> true
                ConsoleLogFilter.DEBUG -> line.level == LogLevel.DEBUG
                ConsoleLogFilter.INFO -> line.level == LogLevel.INFO
                ConsoleLogFilter.WARN -> line.level == LogLevel.WARN
                ConsoleLogFilter.ERROR -> line.level == LogLevel.ERROR
            }
            val queryMatches = searchQuery.isBlank() ||
                line.text.contains(searchQuery, ignoreCase = true) ||
                line.tag.contains(searchQuery, ignoreCase = true)
            levelMatches && queryMatches
        }
    }

    val copyAllLogs: () -> Unit = {
        val fullText = LogBuffer.snapshotText(filteredLogs)
        clipboardManager.setText(AnnotatedString(fullText))
        runCatching { Toast.makeText(context, "Logs copiados", Toast.LENGTH_SHORT).show() }
    }

    LaunchedEffect(filteredLogs.size, autoScroll, isMaximized) {
        if (autoScroll && filteredLogs.isNotEmpty() && scrollState.maxValue in 1 until Int.MAX_VALUE) {
            runCatching { scrollState.animateScrollTo(scrollState.maxValue) }
        }
    }

    Column(
        modifier = modifier
            .fillMaxSize()
            .then(
                if (isMaximized) Modifier else Modifier.padding(horizontal = 12.dp, vertical = 8.dp)
            ),
        verticalArrangement = Arrangement.spacedBy(if (isMaximized) 0.dp else 8.dp)
    ) {
        if (!isMaximized) {
            if (latestCrashText != null) {
                ElevatedCard(
                    colors = CardDefaults.elevatedCardColors(containerColor = Color(0xFFFEF2F2)),
                    shape = RoundedCornerShape(14.dp)
                ) {
                    Column(
                        modifier = Modifier.padding(10.dp),
                        verticalArrangement = Arrangement.spacedBy(4.dp)
                    ) {
                        Text(
                            text = "Crash anterior detectado (${latestCrashFileName ?: "crash.txt"})",
                            color = Color(0xFF991B1B),
                            fontWeight = FontWeight.Bold,
                            fontSize = 12.sp
                        )
                        Text(
                            text = latestCrashText.lineSequence().take(3).joinToString("\n"),
                            color = Color(0xFF7F1D1D),
                            fontFamily = FontFamily.Monospace,
                            fontSize = 11.sp
                        )
                        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                            TextButton(onClick = {
                                clipboardManager.setText(AnnotatedString(latestCrashText))
                                runCatching { Toast.makeText(context, "Crash copiado", Toast.LENGTH_SHORT).show() }
                            }) {
                                Text("Copiar crash", fontSize = 12.sp)
                            }
                            if (onShareCrash != null) {
                                TextButton(onClick = onShareCrash) {
                                    Text("Compartir .txt", fontSize = 12.sp)
                                }
                            }
                            if (onDismissCrash != null) {
                                TextButton(onClick = onDismissCrash) {
                                    Text("Descartar", fontSize = 12.sp)
                                }
                            }
                        }
                    }
                }
            }

            // TarjetaEstado compacta para ceder máximo espacio vertical a la terminal
            ElevatedCard(
                colors = CardDefaults.elevatedCardColors(containerColor = Color.White),
                shape = RoundedCornerShape(14.dp)
            ) {
                Column(
                    modifier = Modifier.padding(horizontal = 12.dp, vertical = 10.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp)
                ) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween,
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Column(modifier = Modifier.weight(1f).padding(end = 8.dp)) {
                            Text(
                                text = config.serverName,
                                style = MaterialTheme.typography.titleSmall,
                                fontWeight = FontWeight.Bold,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis
                            )
                            Text(
                                text = if (isRunning) {
                                    "ACTIVO · ${config.activeLane.label}"
                                } else {
                                    "DETENIDO · ${config.activeLane.label}"
                                },
                                style = MaterialTheme.typography.bodySmall,
                                color = if (isRunning) Color(0xFF047857) else Color(0xFF64748B),
                                fontWeight = FontWeight.SemiBold
                            )
                        }

                        Button(
                            onClick = onStartOrStop,
                            colors = ButtonDefaults.buttonColors(
                                containerColor = if (isRunning) Color(0xFFDC2626) else Color(0xFF047857)
                            ),
                            shape = RoundedCornerShape(10.dp),
                            contentPadding = PaddingValues(horizontal = 12.dp, vertical = 6.dp)
                        ) {
                            Icon(
                                imageVector = if (isRunning) Icons.Default.Close else Icons.Default.PlayArrow,
                                contentDescription = null,
                                modifier = Modifier.size(16.dp)
                            )
                            Spacer(Modifier.width(4.dp))
                            Text(if (isRunning) "Detener" else "Iniciar", fontSize = 12.sp, maxLines = 1)
                        }
                    }

                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(6.dp)
                    ) {
                        EngineLane.entries.forEach { lane ->
                            FilterChip(
                                selected = config.activeLane == lane,
                                onClick = { onSelectLane(lane) },
                                label = {
                                    Text(
                                        text = lane.label,
                                        fontSize = 11.sp,
                                        maxLines = 1,
                                        softWrap = false
                                    )
                                },
                                modifier = Modifier.weight(1f).height(30.dp)
                            )
                        }
                    }

                    val port = if (config.activeLane == EngineLane.JAVA) config.javaPort else config.bedrockPort
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.SpaceBetween
                    ) {
                        Text(
                            "127.0.0.1:$port",
                            fontSize = 11.sp,
                            fontFamily = FontFamily.Monospace,
                            fontWeight = FontWeight.SemiBold,
                            color = Color(0xFF0F172A)
                        )
                        Text(
                            if (hardwareReport != null) {
                                "${hardwareReport.detectedCores}c · ${hardwareReport.ramAvailMb}MB"
                            } else {
                                "SoC pendiente"
                            },
                            fontSize = 11.sp,
                            fontFamily = FontFamily.Monospace,
                            color = Color(0xFF475569)
                        )
                        Text(
                            when {
                                networkReport == null -> "Red pendiente"
                                networkReport.upnpStatus == "igd_gateway_found" -> "UPnP OK"
                                else -> "LAN / CGNAT"
                            },
                            fontSize = 11.sp,
                            fontFamily = FontFamily.Monospace,
                            fontWeight = FontWeight.SemiBold,
                            color = if (networkReport?.upnpStatus == "igd_gateway_found") {
                                Color(0xFF047857)
                            } else {
                                Color(0xFF0F172A)
                            }
                        )
                    }
                }
            }
        }

        // Terminal CLI Interactiva Fase 1.5.2
        Column(
            modifier = Modifier
                .weight(1f)
                .fillMaxWidth()
                .clip(RoundedCornerShape(if (isMaximized) 0.dp else 14.dp))
                .background(Color(0xFF0B1120))
        ) {
            // Cabecera de terminal con Exportar en una sola línea, Limpiar y Pantalla Completa
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(Color(0xFF1E293B))
                    .combinedClickable(
                        onClick = {},
                        onLongClick = copyAllLogs
                    )
                    .padding(horizontal = 10.dp, vertical = 6.dp),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically
            ) {
                Column(modifier = Modifier.weight(1f).padding(end = 6.dp)) {
                    Text(
                        text = "arm64@bedrockdroid:~ (${filteredLogs.size}/${logs.size})",
                        color = Color(0xFFCBD5E1),
                        fontFamily = FontFamily.Monospace,
                        fontSize = 11.sp,
                        fontWeight = FontWeight.SemiBold,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis
                    )
                    Text(
                        text = "Mantén pulsado para copiar todo",
                        color = Color(0xFF94A3B8),
                        fontSize = 10.sp,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis
                    )
                }
                Row(
                    horizontalArrangement = Arrangement.spacedBy(2.dp),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    TextButton(
                        onClick = { LogBuffer.exportLogs(context) },
                        contentPadding = PaddingValues(horizontal = 8.dp, vertical = 4.dp)
                    ) {
                        Icon(
                            imageVector = Icons.Default.Share,
                            contentDescription = "Exportar logs",
                            tint = Color(0xFF34D399),
                            modifier = Modifier.size(16.dp)
                        )
                        Spacer(Modifier.width(4.dp))
                        Text(
                            text = "Exportar",
                            color = Color(0xFF34D399),
                            fontSize = 11.sp,
                            fontWeight = FontWeight.SemiBold,
                            maxLines = 1,
                            softWrap = false
                        )
                    }
                    IconButton(onClick = onClearLogs, modifier = Modifier.size(32.dp)) {
                        Icon(
                            imageVector = Icons.Default.Delete,
                            contentDescription = "Limpiar consola",
                            tint = Color(0xFF94A3B8),
                            modifier = Modifier.size(17.dp)
                        )
                    }
                    IconButton(
                        onClick = { isMaximized = !isMaximized },
                        modifier = Modifier.size(32.dp)
                    ) {
                        Icon(
                            imageVector = if (isMaximized) Icons.Default.FullscreenExit else Icons.Default.Fullscreen,
                            contentDescription = if (isMaximized) "Salir de Pantalla Completa" else "Pantalla Completa",
                            tint = if (isMaximized) Color(0xFF34D399) else Color(0xFFE2E8F0),
                            modifier = Modifier.size(20.dp)
                        )
                    }
                }
            }

            if (!isMaximized) {
                // BarraFiltros: Chips ALL/DEBUG/INFO/WARN/ERROR en horizontalScroll + Auto-scroll + Búsqueda compacta (48.dp)
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .background(Color(0xFF111827))
                        .padding(horizontal = 8.dp, vertical = 6.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp)
                ) {
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.SpaceBetween
                    ) {
                        Row(
                            modifier = Modifier
                                .weight(1f)
                                .horizontalScroll(rememberScrollState()),
                            horizontalArrangement = Arrangement.spacedBy(6.dp)
                        ) {
                            ConsoleLogFilter.entries.forEach { filter ->
                                FilterChip(
                                    selected = activeFilter == filter,
                                    onClick = { activeFilter = filter },
                                    label = {
                                        Text(
                                            text = filter.label,
                                            fontFamily = FontFamily.Monospace,
                                            fontSize = 11.sp,
                                            maxLines = 1,
                                            softWrap = false
                                        )
                                    },
                                    modifier = Modifier.height(32.dp)
                                )
                            }
                        }
                        Row(
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.spacedBy(4.dp),
                            modifier = Modifier.padding(start = 6.dp)
                        ) {
                            Text(
                                text = "Auto",
                                color = Color(0xFFCBD5E1),
                                fontSize = 11.sp,
                                maxLines = 1
                            )
                            Switch(
                                checked = autoScroll,
                                onCheckedChange = { autoScroll = it }
                            )
                        }
                    }

                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .height(48.dp)
                            .clip(RoundedCornerShape(10.dp))
                            .background(Color(0xFF0F172A))
                            .border(1.dp, Color(0xFF334155), RoundedCornerShape(10.dp))
                            .padding(horizontal = 10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(8.dp)
                    ) {
                        Icon(
                            imageVector = Icons.Default.Search,
                            contentDescription = null,
                            tint = Color(0xFF94A3B8),
                            modifier = Modifier.size(16.dp)
                        )
                        Box(modifier = Modifier.weight(1f), contentAlignment = Alignment.CenterStart) {
                            if (searchQuery.isEmpty()) {
                                Text(
                                    text = "Buscar en logs (JNI, PANIC, RakNet)...",
                                    fontFamily = FontFamily.Monospace,
                                    fontSize = 12.sp,
                                    color = Color(0xFF64748B),
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis
                                )
                            }
                            BasicTextField(
                                value = searchQuery,
                                onValueChange = { searchQuery = it },
                                singleLine = true,
                                textStyle = TextStyle(
                                    color = Color.White,
                                    fontFamily = FontFamily.Monospace,
                                    fontSize = 12.sp
                                ),
                                cursorBrush = SolidColor(Color(0xFF10B981)),
                                modifier = Modifier.fillMaxWidth()
                            )
                        }
                        if (searchQuery.isNotEmpty()) {
                            IconButton(onClick = { searchQuery = "" }, modifier = Modifier.size(24.dp)) {
                                Icon(
                                    imageVector = Icons.Default.Close,
                                    contentDescription = "Limpiar búsqueda",
                                    tint = Color(0xFF94A3B8),
                                    modifier = Modifier.size(14.dp)
                                )
                            }
                        }
                    }
                }
            }

            // TerminalLogs(modifier = Modifier.weight(1f)) con SelectionContainer + verticalScroll + FAB Copiar todo
            Box(
                modifier = Modifier
                    .weight(1f)
                    .fillMaxWidth()
            ) {
                SelectionContainer {
                    Column(
                        modifier = Modifier
                            .fillMaxWidth()
                            .verticalScroll(scrollState)
                            .padding(start = 12.dp, end = 12.dp, top = 10.dp, bottom = 58.dp),
                        verticalArrangement = Arrangement.spacedBy(5.dp)
                    ) {
                        filteredLogs.forEach { line ->
                            val lineColor = when (line.stream) {
                                LogStream.CMD -> Color(0xFF34D399)
                                LogStream.DEBUG -> Color(0xFF94A3B8)
                                LogStream.WARN -> Color(0xFFFBBF24)
                                LogStream.ERR -> Color(0xFFF87171)
                                LogStream.NET -> Color(0xFF38BDF8)
                                LogStream.STDOUT -> Color(0xFFE2E8F0)
                            }
                            Text(
                                text = "[${line.time}] [${line.level.badge}] ${line.text}",
                                color = lineColor,
                                fontFamily = FontFamily.Monospace,
                                fontSize = 12.sp,
                                lineHeight = 17.sp
                            )
                        }
                    }
                }

                SmallFloatingActionButton(
                    onClick = copyAllLogs,
                    containerColor = Color(0xFF047857),
                    contentColor = Color.White,
                    modifier = Modifier
                        .align(Alignment.BottomEnd)
                        .padding(12.dp)
                ) {
                    Row(
                        modifier = Modifier.padding(horizontal = 10.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(4.dp)
                    ) {
                        Icon(
                            imageVector = Icons.Default.Check,
                            contentDescription = "Copiar todo",
                            modifier = Modifier.size(16.dp)
                        )
                        Text(
                            text = "Copiar todo",
                            fontSize = 11.sp,
                            fontWeight = FontWeight.SemiBold,
                            maxLines = 1
                        )
                    }
                }
            }

            if (!isMaximized) {
                // BotonesComandos: Chips rápidos + Entrada CLI compacta
                val quickCmds = listOf("./launch hybrid", "./launch bedrock", "./launch java", "netcheck", "soc", "stop", "help")
                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .horizontalScroll(rememberScrollState())
                        .background(Color(0xFF111827))
                        .padding(horizontal = 8.dp, vertical = 4.dp),
                    horizontalArrangement = Arrangement.spacedBy(6.dp)
                ) {
                    quickCmds.forEach { qc ->
                        SuggestionChip(
                            onClick = { onRunCommand(qc) },
                            label = {
                                Text(
                                    text = qc,
                                    fontFamily = FontFamily.Monospace,
                                    fontSize = 11.sp,
                                    color = Color(0xFFE2E8F0),
                                    maxLines = 1,
                                    softWrap = false
                                )
                            },
                            colors = SuggestionChipDefaults.suggestionChipColors(
                                containerColor = Color(0xFF1E293B)
                            ),
                            border = null,
                            modifier = Modifier.height(30.dp)
                        )
                    }
                }

                Row(
                    modifier = Modifier
                        .fillMaxWidth()
                        .background(Color(0xFF0F172A))
                        .padding(horizontal = 8.dp, vertical = 6.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp)
                ) {
                    Row(
                        modifier = Modifier
                            .weight(1f)
                            .height(48.dp)
                            .clip(RoundedCornerShape(10.dp))
                            .background(Color(0xFF111827))
                            .border(1.dp, Color(0xFF334155), RoundedCornerShape(10.dp))
                            .padding(horizontal = 10.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Box(modifier = Modifier.fillMaxWidth(), contentAlignment = Alignment.CenterStart) {
                            if (commandText.isEmpty()) {
                                Text(
                                    text = "Escribe comando CLI (ej. netcheck)...",
                                    fontFamily = FontFamily.Monospace,
                                    fontSize = 12.sp,
                                    color = Color(0xFF64748B),
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis
                                )
                            }
                            BasicTextField(
                                value = commandText,
                                onValueChange = { commandText = it },
                                singleLine = true,
                                textStyle = TextStyle(
                                    color = Color.White,
                                    fontFamily = FontFamily.Monospace,
                                    fontSize = 13.sp
                                ),
                                cursorBrush = SolidColor(Color(0xFF10B981)),
                                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                                keyboardActions = KeyboardActions(
                                    onSend = {
                                        if (commandText.isNotBlank()) {
                                            onRunCommand(commandText)
                                            commandText = ""
                                        }
                                    }
                                ),
                                modifier = Modifier.fillMaxWidth()
                            )
                        }
                    }

                    FilledIconButton(
                        onClick = {
                            if (commandText.isNotBlank()) {
                                onRunCommand(commandText)
                                commandText = ""
                            }
                        },
                        colors = IconButtonDefaults.filledIconButtonColors(
                            containerColor = Color(0xFF047857)
                        ),
                        modifier = Modifier.size(44.dp)
                    ) {
                        Icon(Icons.AutoMirrored.Filled.Send, contentDescription = "Ejecutar comando")
                    }
                }
            }
        }
    }
}
