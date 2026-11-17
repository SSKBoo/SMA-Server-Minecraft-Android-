package com.bedrockdroid.rs.ui

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.provider.Settings
import androidx.core.content.ContextCompat
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.LifecycleOwner
import com.bedrockdroid.rs.engine.EngineLaunchPlanner
import com.bedrockdroid.rs.model.AndroidLauncherConfig
import com.bedrockdroid.rs.model.HardwareScanReport
import com.bedrockdroid.rs.model.LogStream
import com.bedrockdroid.rs.model.NetworkProbeReport
import com.bedrockdroid.rs.model.NetworkStateKind
import com.bedrockdroid.rs.model.ServerRuntimeState
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.util.Locale

fun solicitarExencionBateria(context: Context) {
    try {
        val intent = Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS).apply {
            data = Uri.parse("package:${context.packageName}")
        }
        context.startActivity(intent)
    } catch (e: Exception) {
        runCatching {
            context.startActivity(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS))
        }
    }
}

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
    val isRunningState by ServerRuntimeState.isRunning.collectAsState()
    val serverStatus by ServerRuntimeState.serverStatus.collectAsState()
    val locksAcquired by ServerRuntimeState.locksAcquired.collectAsState()
    val checklistState by ServerRuntimeState.checklistStatus.collectAsState()
    val checklistRefreshTick by ServerRuntimeState.checklistRefreshTick.collectAsState()

    LaunchedEffect(Unit) {
        ServerRuntimeState.refreshChecklist(context)
    }

    LaunchedEffect(isRunning, isRunningState, serverStatus.isRunning, locksAcquired, serverStatus.locksAcquired) {
        ServerRuntimeState.refreshChecklist(context)
    }

    val lifecycleOwner = context as? LifecycleOwner
    DisposableEffect(lifecycleOwner) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_RESUME) {
                ServerRuntimeState.refreshChecklist(context)
            }
        }
        lifecycleOwner?.lifecycle?.addObserver(observer)
        onDispose {
            lifecycleOwner?.lifecycle?.removeObserver(observer)
        }
    }

    val notifLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission()
    ) {
        ServerRuntimeState.refreshChecklist(context)
    }

    val notifGranted = checklistState.notificationsEnabled
    val dozeExempt = checklistState.batteryOptimizationIgnored
    val bgDataUnrestricted = checklistState.backgroundDataUnrestricted
    // Solo PASS (verde) cuando el servidor está corriendo Y todos los locks están activos
    val isServerRunning = isRunning && isRunningState && serverStatus.isRunning
    val wakeLocksPass = isServerRunning && locksAcquired && serverStatus.locksAcquired && checklistState.wakeLocksHeld
    val oemGranted = checklistState.oemRuleConfirmed
    val passCount = listOf(notifGranted, dozeExempt, bgDataUnrestricted, wakeLocksPass, oemGranted).count { it }
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
                    "Analiza frecuencia máxima de núcleos, RAM física total (ActivityManager.MemoryInfo) y almacenamiento para calcular parámetros óptimos.",
                    style = MaterialTheme.typography.bodySmall,
                    color = Color(0xFF475569)
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(
                        onClick = {
                            val report = EngineLaunchPlanner.scanDeviceHardware(context)
                            onScanComplete(report, true)
                            val totalGb = ((report.ramTotalMb + 512) / 1024).coerceAtLeast(1)
                            ServerRuntimeState.appendLog(
                                LogStream.STDOUT,
                                "[Escaneo] ${report.detectedCores} núcleos (${report.primeMaxMhz} MHz) · $totalGb GB totales (${report.ramAvailMb} MB libres) · JVM Xmx ${report.recommendedXmxMb}M · Rust ${report.recommendedRustRamMb}M"
                            )
                        },
                        modifier = Modifier.weight(1f)
                    ) {
                        Text(if (hardwareReport == null) "Autorizar y escanear" else "Re-escanear y aplicar")
                    }
                }
                hardwareReport?.let { hw ->
                    val freeGbText = String.format(Locale.US, "%.1f", hw.storageFreeGb)
                    val totalGb = ((hw.ramTotalMb + 512) / 1024).coerceAtLeast(1)
                    HorizontalDivider(color = Color(0xFFE2E8F0))
                    Text(hw.socLabel, fontWeight = FontWeight.SemiBold, fontSize = 13.sp)
                    Text(
                        "CPU: ${hw.detectedCores} núcleos (Prime ${hw.primeMaxMhz} MHz)\nRAM Física: $totalGb GB totales (${hw.ramTotalMb} MB) · Libre: ${hw.ramAvailMb} MB\nDalvik largeHeap App: ${hw.largeHeapClassMb} MB · Disco libre: $freeGbText GB\nÓptimo aplicado: JVM Xmx ${hw.recommendedXmxMb}M · Rust Nativo ${hw.recommendedRustRamMb}M · MTU ${hw.recommendedMtu}",
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
                    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
                        ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
                        PackageManager.PERMISSION_GRANTED
                    ) {
                        notifLauncher.launch(Manifest.permission.POST_NOTIFICATIONS)
                    } else {
                        val intent = Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).apply {
                            putExtra(Settings.EXTRA_APP_PACKAGE, context.packageName)
                        }
                        runCatching { context.startActivity(intent) }.onFailure {
                            val fallback = Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS).apply {
                                data = Uri.parse("package:${context.packageName}")
                            }
                            runCatching { context.startActivity(fallback) }
                        }
                    }
                    ServerRuntimeState.refreshChecklist(context)
                }
                ChecklistRow("Exención batería (Doze)", dozeExempt) {
                    solicitarExencionBateria(context)
                    ServerRuntimeState.refreshChecklist(context)
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
                    ServerRuntimeState.refreshChecklist(context)
                }
                ChecklistRow("WakeLock + WifiLock + Multicast (al iniciar)", wakeLocksPass) {
                    solicitarExencionBateria(context)
                    ServerRuntimeState.refreshChecklist(context)
                }
                ChecklistRow("Regla OEM (Xiaomi/OneUI/ColorOS)", oemGranted) {
                    val intent = Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS).apply {
                        data = Uri.parse("package:${context.packageName}")
                    }
                    runCatching { context.startActivity(intent) }
                    ServerRuntimeState.setOemRuleConfirmed(true)
                    onConfirmOemRule(true)
                    ServerRuntimeState.refreshChecklist(context)
                }

                OutlinedButton(
                    onClick = {
                        ServerRuntimeState.refreshChecklist(context)
                    },
                    modifier = Modifier.fillMaxWidth()
                ) {
                    Text("Comprobar estado nuevamente")
                }

                if (checklistRefreshTick > 0L) {
                    Text(
                        text = if (isServerRunning) "Estado en vivo verificado (servidor activo)" else "Servidor detenido: locks 24/7 en espera (PENDIENTE)",
                        fontSize = 11.sp,
                        fontFamily = FontFamily.Monospace,
                        color = Color(0xFF64748B)
                    )
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
                    val (stateBg, stateColor, stateIcon, stateTitle) = when (net.networkState) {
                        NetworkStateKind.UPNP_OK -> Quad(Color(0xFFECFDF5), Color(0xFF047857), Icons.Default.CheckCircle, "Estado: upnp_ok · Puerto público listo")
                        NetworkStateKind.CGNAT -> Quad(Color(0xFFFFFBEB), Color(0xFFB45309), Icons.Default.Warning, "Estado: cgnat · Usa túnel (playit.gg / Tailscale)")
                        NetworkStateKind.LOCAL_ONLY -> Quad(Color(0xFFF1F5F9), Color(0xFF334155), Icons.Default.Info, "Estado: local_only · Sin UPnP IGD en router")
                    }
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clip(RoundedCornerShape(10.dp))
                            .background(stateBg)
                            .border(1.dp, stateColor.copy(alpha = 0.25f), RoundedCornerShape(10.dp))
                            .padding(horizontal = 10.dp, vertical = 8.dp),
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(8.dp)
                    ) {
                        Icon(
                            imageVector = stateIcon,
                            contentDescription = net.networkState.id,
                            tint = stateColor,
                            modifier = Modifier.size(18.dp)
                        )
                        Text(
                            text = stateTitle,
                            fontSize = 12.sp,
                            fontWeight = FontWeight.SemiBold,
                            color = stateColor
                        )
                    }
                    Text(
                        "Loopback: ${net.loopbackHint}\nLAN Wi-Fi: ${net.lanIp}\nIP Pública (ipify): ${net.publicIp ?: "no disponible"}\nUPnP IGD: ${net.upnpStatus}${if (net.igdWanIp != null) " (WAN ${net.igdWanIp})" else ""}\nBind: v4=${net.bindV4} · v6=${net.bindV6} (v6only=${net.ipv6V6Only})",
                        fontFamily = FontFamily.Monospace,
                        fontSize = 12.sp
                    )
                    Text(net.guidance, fontSize = 12.sp, color = Color(0xFF475569))
                }
            }
        }
    }
}

private data class Quad<A, B, C, D>(val first: A, val second: B, val third: C, val fourth: D)

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
                fontWeight = if (passed) FontWeight.Bold else FontWeight.Medium,
                color = if (passed) Color(0xFF047857) else Color(0xFF64748B)
            )
        }
        if (!passed) {
            TextButton(onClick = onAction) {
                Text("Configurar")
            }
        }
    }
}
