package com.bedrockdroid.rs.ui

import android.app.ActivityManager
import android.content.Context
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.bedrockdroid.rs.model.AndroidLauncherConfig
import com.bedrockdroid.rs.model.ServerRuntimeState

@Composable
fun SettingsScreen(
    config: AndroidLauncherConfig,
    onUpdateConfig: (AndroidLauncherConfig) -> Unit,
    modifier: Modifier = Modifier
) {
    val context = LocalContext.current
    val hardwareReport by ServerRuntimeState.hardwareReport.collectAsState()

    val (totalRamMb, largeHeapMb) = remember(hardwareReport) {
        val am = context.getSystemService(Context.ACTIVITY_SERVICE) as? ActivityManager
        val largeClass = hardwareReport?.largeHeapClassMb
            ?: runCatching { am?.largeMemoryClass ?: 512 }.getOrDefault(512).coerceAtLeast(256)
        val totalMb = hardwareReport?.ramTotalMb ?: runCatching {
            val memInfo = ActivityManager.MemoryInfo()
            am?.getMemoryInfo(memInfo)
            (memInfo.totalMem / (1024L * 1024L)).toInt()
        }.getOrDefault(8192).coerceAtLeast(2048)
        totalMb to largeClass
    }

    val totalRamGb = ((totalRamMb + 512) / 1024).coerceAtLeast(1)
    // Límite estricto Dalvik/ART: la JVM no debe superar ActivityManager.getLargeMemoryClass()
    val jvmSliderMaxMb = largeHeapMb.coerceAtLeast(256)
    val jvmSliderMinMb = 256.coerceAtMost(jvmSliderMaxMb)
    val effectiveJvmXmx = config.jvmXmxMb.coerceIn(jvmSliderMinMb, jvmSliderMaxMb)
    val effectiveJvmXms = config.jvmXmsMb.coerceIn(128.coerceAtMost(effectiveJvmXmx), effectiveJvmXmx)

    // El motor Rust nativo no está sujeto a Dalvik y puede usar hasta el 85% de la RAM física real
    val rustSliderMaxMb = (((totalRamMb * 0.85).toInt() / 512) * 512).coerceAtLeast(1024)
    val effectiveRustRamMb = config.rustMaxRamMb.coerceIn(1024, rustSliderMaxMb)

    Column(
        modifier = modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp)
    ) {
        Text(
            text = "Ajustes de Red, JVM y Snapdragon",
            style = MaterialTheme.typography.titleLarge,
            fontWeight = FontWeight.Bold
        )

        ElevatedCard(
            colors = CardDefaults.elevatedCardColors(containerColor = Color.White),
            shape = RoundedCornerShape(16.dp)
        ) {
            Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text("Servidor y Red Local/WAN", fontWeight = FontWeight.Bold)
                OutlinedTextField(
                    value = config.serverName,
                    onValueChange = { onUpdateConfig(config.copy(serverName = it)) },
                    label = { Text("Nombre del servidor") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth()
                )
                var portInput by remember(config.bedrockPort) { mutableStateOf(config.bedrockPort.toString()) }
                LaunchedEffect(config.bedrockPort) {
                    if (portInput != config.bedrockPort.toString()) {
                        portInput = config.bedrockPort.toString()
                    }
                }
                OutlinedTextField(
                    value = portInput,
                    onValueChange = { raw ->
                        val digits = raw.filter { it.isDigit() }.take(5)
                        portInput = digits
                        val parsed = digits.toIntOrNull()
                        if (parsed != null && parsed in 1024..65535) {
                            onUpdateConfig(config.copy(bedrockPort = parsed))
                        }
                    },
                    label = { Text("Puerto UDP Bedrock (19132 / 19133 / 25566)") },
                    supportingText = {
                        Text("Si tu ROM bloquea el 19132, prueba 19133 o 25566 y reinicia el servidor.")
                    },
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth()
                )
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp)
                ) {
                    listOf(19132, 19133, 25566).forEach { presetPort ->
                        FilterChip(
                            selected = config.bedrockPort == presetPort,
                            onClick = {
                                portInput = presetPort.toString()
                                onUpdateConfig(config.copy(bedrockPort = presetPort))
                            },
                            label = {
                                Text(
                                    text = if (presetPort == 19132) "19132 (Std)" else presetPort.toString(),
                                    fontFamily = FontFamily.Monospace,
                                    fontSize = 11.sp
                                )
                            }
                        )
                    }
                }
                SettingSwitch("Fast-Path Local (<1 ms en 127.0.0.1/LAN)", config.localFastPath) {
                    onUpdateConfig(config.copy(localFastPath = it))
                }
                SettingSwitch("TCP_NODELAY + IP_TOS baja latencia", config.tcpNoDelay) {
                    onUpdateConfig(config.copy(tcpNoDelay = it, ipTosLowDelay = it))
                }
                SettingSwitch("Anuncio mDNS LAN (_minecraft._udp)", config.enableMdns) {
                    onUpdateConfig(config.copy(enableMdns = it))
                }
                SettingSwitch("Apertura automática UPnP IGD en Wi-Fi", config.enableUpnp) {
                    onUpdateConfig(config.copy(enableUpnp = it))
                }
                SettingSwitch("Dual-Stack IPv4 + IPv6", config.dualStackIpv6) {
                    onUpdateConfig(config.copy(dualStackIpv6 = it))
                }
            }
        }

        ElevatedCard(
            colors = CardDefaults.elevatedCardColors(containerColor = Color.White),
            shape = RoundedCornerShape(16.dp)
        ) {
            Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text("Memoria RAM (JVM ARM64 vs Rust Nativo)", fontWeight = FontWeight.Bold)
                Text(
                    "RAM Física: $totalRamGb GB totales ($totalRamMb MB) · Límite Dalvik (getLargeMemoryClass): $jvmSliderMaxMb MB",
                    fontFamily = FontFamily.Monospace,
                    fontSize = 12.sp,
                    color = Color(0xFF047857)
                )
                Text(
                    "La JVM (Java) tiene un límite estricto impuesto por Android (Dalvik/ART) y no puede superar getLargeMemoryClass() ($jvmSliderMaxMb MB) sin riesgo de cierre. El motor Rust (Bedrock Nativo) corre fuera de Dalvik y puede usar hasta el 85% de tu RAM física ($rustSliderMaxMb MB).",
                    style = MaterialTheme.typography.bodySmall,
                    color = Color(0xFF475569)
                )

                HorizontalDivider(color = Color(0xFFE2E8F0))

                Text(
                    "JVM Java / Híbrido — Xms $effectiveJvmXms MB · Xmx $effectiveJvmXmx MB (máx. estricto $jvmSliderMaxMb MB)",
                    fontFamily = FontFamily.Monospace,
                    fontSize = 12.sp
                )
                Slider(
                    value = effectiveJvmXmx.toFloat(),
                    onValueChange = {
                        val nextXmx = ((it.toInt() / 64) * 64).coerceIn(jvmSliderMinMb, jvmSliderMaxMb)
                        val nextXms = config.jvmXmsMb.coerceAtMost(nextXmx)
                        onUpdateConfig(config.copy(jvmXmxMb = nextXmx, jvmXmsMb = nextXms))
                    },
                    valueRange = jvmSliderMinMb.toFloat()..jvmSliderMaxMb.toFloat()
                )

                val rustGbText = String.format(java.util.Locale.US, "%.1f", effectiveRustRamMb / 1024.0)
                val rustMaxGbText = String.format(java.util.Locale.US, "%.1f", rustSliderMaxMb / 1024.0)
                Text(
                    "Motor Rust (Bedrock Nativo) — Límite $effectiveRustRamMb MB (~$rustGbText GB · máx. 85% = $rustMaxGbText GB)",
                    fontFamily = FontFamily.Monospace,
                    fontSize = 12.sp
                )
                Slider(
                    value = effectiveRustRamMb.toFloat(),
                    onValueChange = {
                        val nextRustMb = ((it.toInt() / 512) * 512).coerceIn(1024, rustSliderMaxMb)
                        onUpdateConfig(config.copy(rustMaxRamMb = nextRustMb))
                    },
                    valueRange = 1024f..rustSliderMaxMb.toFloat()
                )

                SettingSwitch("Aceptar EULA automáticamente", config.eulaAccepted) {
                    onUpdateConfig(config.copy(eulaAccepted = it))
                }
                SettingSwitch("Afinidad de núcleos Snapdragon (Perf/Prime)", config.snapdragonPinning) {
                    onUpdateConfig(config.copy(snapdragonPinning = it))
                }
                SettingSwitch("Prioridad I/O de chunks (ioprio BE)", config.ioPriorityBe) {
                    onUpdateConfig(config.copy(ioPriorityBe = it))
                }
            }
        }
    }
}

@Composable
private fun SettingSwitch(label: String, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically
    ) {
        Text(label, fontSize = 14.sp, modifier = Modifier.weight(1f).padding(end = 12.dp))
        Switch(checked = checked, onCheckedChange = onChange)
    }
}
