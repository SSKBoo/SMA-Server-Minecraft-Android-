package com.bedrockdroid.rs.ui

import android.widget.Toast
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Share
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
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

    LaunchedEffect(filteredLogs.size, autoScroll) {
        if (autoScroll && filteredLogs.isNotEmpty() && scrollState.maxValue in 1 until Int.MAX_VALUE) {
            runCatching { scrollState.animateScrollTo(scrollState.maxValue) }
        }
    }

    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(horizontal = 16.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp)
    ) {
        if (latestCrashText != null) {
            ElevatedCard(
                colors = CardDefaults.elevatedCardColors(containerColor = Color(0xFFFEF2F2)),
                shape = RoundedCornerShape(14.dp)
            ) {
                Column(
                    modifier = Modifier.padding(12.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp)
                ) {
                    Text(
                        text = "Crash anterior detectado (${latestCrashFileName ?: "crash.txt"})",
                        color = Color(0xFF991B1B),
                        fontWeight = FontWeight.Bold,
                        fontSize = 13.sp
                    )
                    Text(
                        text = latestCrashText.lineSequence().take(4).joinToString("\n"),
                        color = Color(0xFF7F1D1D),
                        fontFamily = FontFamily.Monospace,
                        fontSize = 11.sp
                    )
                    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
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

        // Tarjeta de control rápido del servidor
        ElevatedCard(
            colors = CardDefaults.elevatedCardColors(containerColor = Color.White),
            shape = RoundedCornerShape(16.dp)
        ) {
            Column(
                modifier = Modifier.padding(14.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp)
            ) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Column(modifier = Modifier.weight(1f)) {
                        Text(
                            text = config.serverName,
                            style = MaterialTheme.typography.titleMedium,
                            fontWeight = FontWeight.Bold
                        )
                        Text(
                            text = if (isRunning) {
                                "ACTIVO · Carril ${config.activeLane.label}"
                            } else {
                                "DETENIDO · Listo en ${config.activeLane.label}"
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
                        shape = RoundedCornerShape(12.dp)
                    ) {
                        Icon(
                            imageVector = if (isRunning) Icons.Default.Close else Icons.Default.PlayArrow,
                            contentDescription = null,
                            modifier = Modifier.size(18.dp)
                        )
                        Spacer(Modifier.width(6.dp))
                        Text(if (isRunning) "Detener" else "Iniciar")
                    }
                }

                // Selector rápido de los 3 carriles
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp)
                ) {
                    EngineLane.entries.forEach { lane ->
                        FilterChip(
                            selected = config.activeLane == lane,
                            onClick = { onSelectLane(lane) },
                            label = {
                                Text(
                                    text = lane.label,
                                    fontSize = 12.sp,
                                    maxLines = 1
                                )
                            },
                            modifier = Modifier.weight(1f)
                        )
                    }
                }

                // Resumen legible SoC y Red
                HorizontalDivider(color = Color(0xFFE2E8F0))
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween
                ) {
                    val port = if (config.activeLane == EngineLane.JAVA) config.javaPort else config.bedrockPort
                    Column {
                        Text("Local Fast-Path", fontSize = 11.sp, color = Color(0xFF64748B))
                        Text(
                            "127.0.0.1:$port",
                            fontSize = 12.sp,
                            fontFamily = FontFamily.Monospace,
                            fontWeight = FontWeight.SemiBold
                        )
                    }
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        Text("Hardware", fontSize = 11.sp, color = Color(0xFF64748B))
                        Text(
                            if (hardwareReport != null) {
                                "${hardwareReport.detectedCores} núcleos · ${hardwareReport.ramAvailMb}MB"
                            } else {
                                "Sin escanear"
                            },
                            fontSize = 12.sp,
                            fontFamily = FontFamily.Monospace,
                            fontWeight = FontWeight.SemiBold
                        )
                    }
                    Column(horizontalAlignment = Alignment.End) {
                        Text("Red / WAN", fontSize = 11.sp, color = Color(0xFF64748B))
                        Text(
                            when {
                                networkReport == null -> "Pendiente"
                                networkReport.upnpStatus == "igd_gateway_found" -> "UPnP OK"
                                else -> "LAN / CGNAT"
                            },
                            fontSize = 12.sp,
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

        // Terminal CLI Interactiva Fase 1.5
        Column(
            modifier = Modifier
                .weight(1f)
                .fillMaxWidth()
                .clip(RoundedCornerShape(16.dp))
                .background(Color(0xFF0B1120))
        ) {
            // Cabecera de terminal con Exportar .log, Copiar (LongClick) y Limpiar
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(Color(0xFF1E293B))
                    .combinedClickable(
                        onClick = {},
                        onLongClick = copyAllLogs
                    )
                    .padding(horizontal = 12.dp, vertical = 6.dp),
                horizontalArrangement = Arrangement.SpaceBetween,
                verticalAlignment = Alignment.CenterVertically
            ) {
                Column {
                    Text(
                        text = "arm64@bedrockdroid:~ (${filteredLogs.size}/${logs.size})",
                        color = Color(0xFFCBD5E1),
                        fontFamily = FontFamily.Monospace,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.SemiBold
                    )
                    Text(
                        text = "Mantén pulsado aquí para copiar todo",
                        color = Color(0xFF94A3B8),
                        fontSize = 10.sp
                    )
                }
                Row(
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    TextButton(
                        onClick = { LogBuffer.exportLogs(context) },
                        contentPadding = PaddingValues(horizontal = 10.dp, vertical = 4.dp)
                    ) {
                        Icon(
                            imageVector = Icons.Default.Share,
                            contentDescription = "Exportar logs",
                            tint = Color(0xFF34D399),
                            modifier = Modifier.size(16.dp)
                        )
                        Spacer(Modifier.width(4.dp))
                        Text(
                            text = "Exportar .log",
                            color = Color(0xFF34D399),
                            fontSize = 11.sp,
                            fontWeight = FontWeight.SemiBold
                        )
                    }
                    IconButton(onClick = onClearLogs, modifier = Modifier.size(28.dp)) {
                        Icon(
                            imageVector = Icons.Default.Delete,
                            contentDescription = "Limpiar consola",
                            tint = Color(0xFF94A3B8),
                            modifier = Modifier.size(16.dp)
                        )
                    }
                }
            }

            // Filtros por nivel + Auto-scroll + Búsqueda
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(Color(0xFF111827))
                    .padding(horizontal = 10.dp, vertical = 6.dp),
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
                                        fontSize = 11.sp
                                    )
                                }
                            )
                        }
                    }
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(4.dp),
                        modifier = Modifier.padding(start = 6.dp)
                    ) {
                        Text(
                            text = "Auto-scroll",
                            color = Color(0xFFCBD5E1),
                            fontSize = 11.sp
                        )
                        Switch(
                            checked = autoScroll,
                            onCheckedChange = { autoScroll = it }
                        )
                    }
                }

                OutlinedTextField(
                    value = searchQuery,
                    onValueChange = { searchQuery = it },
                    placeholder = {
                        Text(
                            "Buscar en logs (ej. JNI, PANIC, RakNet)...",
                            fontFamily = FontFamily.Monospace,
                            fontSize = 11.sp,
                            color = Color(0xFF64748B)
                        )
                    },
                    leadingIcon = {
                        Icon(
                            imageVector = Icons.Default.Search,
                            contentDescription = null,
                            tint = Color(0xFF94A3B8),
                            modifier = Modifier.size(16.dp)
                        )
                    },
                    singleLine = true,
                    textStyle = LocalTextStyle.current.copy(
                        color = Color.White,
                        fontFamily = FontFamily.Monospace,
                        fontSize = 12.sp
                    ),
                    colors = OutlinedTextFieldDefaults.colors(
                        focusedBorderColor = Color(0xFF10B981),
                        unfocusedBorderColor = Color(0xFF334155),
                        cursorColor = Color(0xFF10B981)
                    ),
                    modifier = Modifier.fillMaxWidth()
                )
            }

            // Visor interactivo: SelectionContainer + verticalScroll + FAB Copiar todo (sin combinedClickable que compita con selección)
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
                            fontWeight = FontWeight.SemiBold
                        )
                    }
                }
            }

            // Chips rápidos de comandos táctiles
            val quickCmds = listOf("./launch hybrid", "./launch bedrock", "./launch java", "netcheck", "soc", "stop", "help")
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .horizontalScroll(rememberScrollState())
                    .background(Color(0xFF111827))
                    .padding(horizontal = 10.dp, vertical = 6.dp),
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
                                color = Color(0xFFE2E8F0)
                            )
                        },
                        colors = SuggestionChipDefaults.suggestionChipColors(
                            containerColor = Color(0xFF1E293B)
                        ),
                        border = null
                    )
                }
            }

            // Campo de entrada CLI
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .background(Color(0xFF0F172A))
                    .padding(horizontal = 10.dp, vertical = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp)
            ) {
                OutlinedTextField(
                    value = commandText,
                    onValueChange = { commandText = it },
                    placeholder = {
                        Text(
                            "Escribe comando CLI (ej. netcheck)...",
                            fontFamily = FontFamily.Monospace,
                            fontSize = 12.sp,
                            color = Color(0xFF64748B)
                        )
                    },
                    singleLine = true,
                    textStyle = LocalTextStyle.current.copy(
                        color = Color.White,
                        fontFamily = FontFamily.Monospace,
                        fontSize = 13.sp
                    ),
                    keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                    keyboardActions = KeyboardActions(
                        onSend = {
                            if (commandText.isNotBlank()) {
                                onRunCommand(commandText)
                                commandText = ""
                            }
                        }
                    ),
                    colors = OutlinedTextFieldDefaults.colors(
                        focusedBorderColor = Color(0xFF10B981),
                        unfocusedBorderColor = Color(0xFF334155),
                        cursorColor = Color(0xFF10B981)
                    ),
                    modifier = Modifier.weight(1f)
                )

                FilledIconButton(
                    onClick = {
                        if (commandText.isNotBlank()) {
                            onRunCommand(commandText)
                            commandText = ""
                        }
                    },
                    colors = IconButtonDefaults.filledIconButtonColors(
                        containerColor = Color(0xFF047857)
                    )
                ) {
                    Icon(Icons.AutoMirrored.Filled.Send, contentDescription = "Ejecutar comando")
                }
            }
        }
    }
}
