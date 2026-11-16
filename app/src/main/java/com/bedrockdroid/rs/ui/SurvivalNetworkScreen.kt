package com.bedrockdroid.rs.ui

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.ConnectivityManager
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import com.bedrockdroid.rs.engine.EngineLaunchPlanner
import com.bedrockdroid.rs.model.AndroidLauncherConfig
import com.bedrockdroid.rs.model.HardwareScanReport
import com.bedrockdroid.rs.model.LogStream
import com.bedrockdroid.rs.model.NetworkProbeReport
import com.bedrockdroid.rs.model.ServerRuntimeState
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.util.Locale

@Composable
fun SurvivalNetworkScreen(
    config: AndroidLauncherConfig,
    isRunning: Boolean,
    hardwareReport: HardwareScanReport?,
    networkReport: NetworkProbeReport?,
    onScanComplete: (HardwareScanReport, Boolean) -> Unit,
    onConfirmOemRule: (Boolean) -> Unit,
    onNetworkProbeComplete: (NetworkProbeReport) -> Unit,
    modifier: Modifier = Modifier
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var refreshTrigger by remember { mutableIntStateOf(0) }

    val notifLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) { refreshTrigger++ }

    val notifGranted = remember(refreshTrigger) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
                PackageManager.PERMISSION_GRANTED
        } else {
            true
        }
    }

    val dozeExempt = remember(refreshTrigger) {
        val pm = context.getSystemService(Context.POWER_SERVICE) as PowerManager
        pm.isIgnoringBatteryOptimizations(context.packageName)
    }

    val bgDataUnrestricted = remember(refreshTrigger) {
        val cm = context.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
        cm.restrictBackgroundStatus != ConnectivityManager.RESTRICT_BACKGROUND_STATUS_ENABLED
    }

    val oemGranted = config.oemAutostartConfirmed
    val passCount = listOf(notifGranted, dozeExempt, bgDataUnrestricted, isRunning, oemGranted).count { it }
    val checklistBadge = if (passCount == 5) "$passCount/5 PASS" else "$passCount/5 PENDIENTE"

    Column(
        modifier = modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp)
    ) {
        // 1. Escaneo consentido del celular
        ElevatedCard(
            colors = CardDefaults.elevatedCardColors(containerColor = Color.White),
            shape = RoundedCornerShape(16.dp)
        ) {
            Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text("1. Escaneo Consentido Snapdragon / ARM64", fontWeight = FontWeight.Bold)
                Text(
                    "Analiza frecuencia máxima de núcleos, RAM disponible y almacenamiento para calcular parámetros óptimos.",
                    style = MaterialTheme.typography.bodySmall,
                    color = Color(0xFF475569)
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(
                        onClick = {
                            val report = EngineLaunchPlanner.scanDeviceHardware(context)
                            onScanComplete(report, true)
                            ServerRuntimeState.appendLog(
                                LogStream.STDOUT,
                                "[Escaneo] ${report.detectedCores} núcleos (${report.primeMaxMhz} MHz) · Xmx recomendado ${report.recommendedXmxMb}M"
                            )
                        },
                        modifier = Modifier.weight(1f)
                    ) {
                        Text(if (hardwareReport == null) "Autorizar y escanear" else "Re-escanear y aplicar")
                    }
                }
                hardwareReport?.let { hw ->
                    val freeGbText = String.format(Locale.US, "%.1f", hw.storageFreeGb)
                    HorizontalDivider(color = Color(0xFFE2E8F0))
                    Text(hw.socLabel, fontWeight = FontWeight.SemiBold, fontSize = 13.sp)
                    Text(
                        "CPU: ${hw.detectedCores} núcleos (Prime ${hw.primeMaxMhz} MHz)\nRAM libre: ${hw.ramAvailMb} / ${hw.ramTotalMb} MB · Disco libre: $freeGbText GB\nÓptimo aplicado: Xms ${hw.recommendedXmsMb}M / Xmx ${hw.recommendedXmxMb}M · MTU ${hw.recommendedMtu}",
                        fontFamily = FontFamily.Monospace,
                        fontSize = 12.sp,
                        color = Color(0xFF047857)
                    )
                }
            }
        }

        // 2. Checklist Supervivencia 24/7
        ElevatedCard(
            colors = CardDefaults.elevatedCardColors(containerColor = Color.White),
            shape = RoundedCornerShape(16.dp)
        ) {
            Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Text("2. Checklist Supervivencia 24/7", fontWeight = FontWeight.Bold)
                    Text(
                        checklistBadge,
                        fontFamily = FontFamily.Monospace,
                        fontSize = 12.sp,
                        fontWeight = FontWeight.Bold,
                        color = if (passCount == 5) Color(0xFF047857) else Color(0xFFD97706)
                    )
                }

                LinearProgressIndicator(
                    progress = { passCount / 5f },
                    modifier = Modifier.fillMaxWidth()
                )

                ChecklistRow("Notificaciones persistentes", notifGranted) {
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                        notifLauncher.launch(Manifest.permission.POST_NOTIFICATIONS)
                    }
                }
                ChecklistRow("Exención batería (Doze)", dozeExempt) {
                    val intent = Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS).apply {
                        data = Uri.parse("package:${context.packageName}")
                    }
                    runCatching { context.startActivity(intent) }
                }
                ChecklistRow("Datos segundo plano sin límite", bgDataUnrestricted) {
                    val intent = Intent(Settings.ACTION_IGNORE_BACKGROUND_DATA_RESTRICTIONS_SETTINGS).apply {
                        data = Uri.parse("package:${context.packageName}")
                    }
                    runCatching { context.startActivity(intent) }.onFailure {
                        val fallback = Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS).apply {
                            data = Uri.parse("package:${context.packageName}")
                        }
                        runCatching { context.startActivity(fallback) }
                    }
                }
                ChecklistRow("WakeLock + WifiLock + Multicast (al iniciar)", isRunning) {
                    val intent = Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS).apply {
                        data = Uri.parse("package:${context.packageName}")
                    }
                    runCatching { context.startActivity(intent) }.onFailure {
                        val fallback = Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS)
                        runCatching { context.startActivity(fallback) }
                    }
                    refreshTrigger++
                }
                ChecklistRow("Regla OEM (Xiaomi/OneUI/ColorOS)", oemGranted) {
                    val intent = Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS).apply {
                        data = Uri.parse("package:${context.packageName}")
                    }
                    runCatching { context.startActivity(intent) }
                    onConfirmOemRule(true)
                }

                OutlinedButton(
                    onClick = { refreshTrigger++ },
                    modifier = Modifier.fillMaxWidth()
                ) {
                    Text("Comprobar estado nuevamente")
                }
            }
        }

        // 3. Red Local (<1 ms) + Internet WAN
        ElevatedCard(
            colors = CardDefaults.elevatedCardColors(containerColor = Color.White),
            shape = RoundedCornerShape(16.dp)
        ) {
            Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text("3. Red Local (<1 ms) e Internet WAN", fontWeight = FontWeight.Bold)
                Button(
                    onClick = {
                        scope.launch {
                            val probe = withContext(Dispatchers.IO) {
                                EngineLaunchPlanner.probeNetwork(config)
                            }
                            onNetworkProbeComplete(probe)
                            ServerRuntimeState.appendLog(
                                LogStream.NET,
                                "[Red] Local=${probe.loopbackHint} LAN=${probe.lanIp} UPnP=${probe.upnpStatus}"
                            )
                        }
                    },
                    modifier = Modifier.fillMaxWidth()
                ) {
                    Text("Probar conexión Local + UPnP / CGNAT")
                }
                networkReport?.let { net ->
                    Text(
                        "Loopback: ${net.loopbackHint}\nLAN Wi-Fi: ${net.lanIp}\nUPnP IGD: ${net.upnpStatus}",
                        fontFamily = FontFamily.Monospace,
                        fontSize = 12.sp
                    )
                    Text(net.guidance, fontSize = 12.sp, color = Color(0xFF475569))
                }
            }
        }
    }
}

@Composable
private fun ChecklistRow(title: String, passed: Boolean, onAction: () -> Unit) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.SpaceBetween,
        verticalAlignment = Alignment.CenterVertically
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(title, fontSize = 13.sp, fontWeight = FontWeight.Medium)
            Text(
                if (passed) "PASS" else "PENDIENTE",
                fontSize = 11.sp,
                fontFamily = FontFamily.Monospace,
                color = if (passed) Color(0xFF047857) else Color(0xFFD97706)
            )
        }
        if (!passed) {
            TextButton(onClick = onAction) {
                Text("Configurar")
            }
        }
    }
}
