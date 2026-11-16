package com.bedrockdroid.rs.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.bedrockdroid.rs.model.AndroidLauncherConfig

@Composable
fun SettingsScreen(
    config: AndroidLauncherConfig,
    onUpdateConfig: (AndroidLauncherConfig) -> Unit,
    modifier: Modifier = Modifier
) {
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
                Text("JVM ARM64 y Snapdragon", fontWeight = FontWeight.Bold)
                Text(
                    "Heap: Xms ${config.jvmXmsMb} MB · Xmx ${config.jvmXmxMb} MB",
                    fontFamily = FontFamily.Monospace,
                    fontSize = 13.sp
                )
                Slider(
                    value = config.jvmXmxMb.toFloat(),
                    onValueChange = {
                        val nextXmx = ((it.toInt() / 256) * 256).coerceIn(768, 4096)
                        onUpdateConfig(config.copy(jvmXmxMb = nextXmx))
                    },
                    valueRange = 768f..4096f
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
