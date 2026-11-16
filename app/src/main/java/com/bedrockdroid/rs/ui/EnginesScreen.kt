package com.bedrockdroid.rs.ui

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
import com.bedrockdroid.rs.engine.EngineLaunchPlanner
import com.bedrockdroid.rs.model.AndroidLauncherConfig
import com.bedrockdroid.rs.model.EngineLane
import com.bedrockdroid.rs.model.JavaFlavor
import com.bedrockdroid.rs.model.LogStream
import com.bedrockdroid.rs.model.ServerRuntimeState
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

@Composable
fun EnginesScreen(
    config: AndroidLauncherConfig,
    onUpdateConfig: (AndroidLauncherConfig) -> Unit,
    modifier: Modifier = Modifier
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var apiCheckStatus by remember { mutableStateOf<String?>(null) }
    var hybridNotice by remember { mutableStateOf<String?>(null) }

    Column(
        modifier = modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp)
    ) {
        Text(
            text = "Convertidor Modular (3 Carriles)",
            style = MaterialTheme.typography.titleLarge,
            fontWeight = FontWeight.Bold
        )
        Text(
            text = "Bedrock y Java se almacenan por separado. El perfil Híbrido se crea aparte sin alterar las bases.",
            style = MaterialTheme.typography.bodySmall,
            color = Color(0xFF475569)
        )

        // Carril 1: Bedrock Nativo
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
                    Text("1. Bedrock Nativo (RakNet)", fontWeight = FontWeight.Bold)
                    TextButton(onClick = { onUpdateConfig(config.copy(activeLane = EngineLane.BEDROCK)) }) {
                        Text(if (config.activeLane == EngineLane.BEDROCK) "Activo" else "Seleccionar")
                    }
                }
                Text(
                    "Versión: ${config.bedrockVersion} · Puerto UDP: ${config.bedrockPort}",
                    fontFamily = FontFamily.Monospace,
                    fontSize = 12.sp
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    listOf("26.60", "26.63", "1.21.51", "1.21.44").forEach { ver ->
                        FilterChip(
                            selected = config.bedrockVersion == ver,
                            onClick = { onUpdateConfig(config.copy(bedrockVersion = ver)) },
                            label = { Text(ver) }
                        )
                    }
                }
            }
        }

        // Carril 2: Java Edition
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
                    Text("2. Java Edition ARM64", fontWeight = FontWeight.Bold)
                    TextButton(onClick = { onUpdateConfig(config.copy(activeLane = EngineLane.JAVA)) }) {
                        Text(if (config.activeLane == EngineLane.JAVA) "Activo" else "Seleccionar")
                    }
                }
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    JavaFlavor.entries.forEach { fl ->
                        FilterChip(
                            selected = config.javaFlavor == fl,
                            onClick = { onUpdateConfig(config.copy(javaFlavor = fl)) },
                            label = { Text(fl.label, fontSize = 12.sp) }
                        )
                    }
                }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    listOf("1.21.4", "1.21.1", "1.20.6").forEach { ver ->
                        FilterChip(
                            selected = config.javaVersion == ver,
                            onClick = { onUpdateConfig(config.copy(javaVersion = ver)) },
                            label = { Text(ver, fontSize = 12.sp) }
                        )
                    }
                }
                Text(
                    "Versión: ${config.javaVersion} · Puerto TCP: ${config.javaPort}",
                    fontFamily = FontFamily.Monospace,
                    fontSize = 12.sp
                )
                OutlinedButton(
                    onClick = {
                        scope.launch {
                            apiCheckStatus = "Consultando API oficial..."
                            val url = EngineLaunchPlanner.officialEndpoints(config.javaVersion)[config.javaFlavor.name.lowercase()]
                                ?: "https://api.papermc.io/v2/projects/paper"
                            val res = withContext(Dispatchers.IO) { EngineLaunchPlanner.checkEndpointReachable(url) }
                            apiCheckStatus = "${config.javaFlavor.label} (${config.javaVersion}): $res"
                            ServerRuntimeState.appendLog(LogStream.NET, "[API] $apiCheckStatus")
                        }
                    }
                ) {
                    Text("Verificar endpoint HTTPS oficial")
                }
                apiCheckStatus?.let {
                    Text(it, fontSize = 12.sp, fontFamily = FontFamily.Monospace, color = Color(0xFF047857))
                }
            }
        }

        // Carril 3: Híbrido (Copia aparte)
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
                    Text(
                        "3. Híbrido (Java + GeyserMC + Floodgate)",
                        fontWeight = FontWeight.Bold,
                        modifier = Modifier.weight(1f)
                    )
                    TextButton(onClick = { onUpdateConfig(config.copy(activeLane = EngineLane.HYBRID)) }) {
                        Text(if (config.activeLane == EngineLane.HYBRID) "Activo" else "Seleccionar")
                    }
                }
                Text(
                    "Geyser ${config.hybridGeyserVersion} sobre Java ${config.javaVersion}",
                    fontFamily = FontFamily.Monospace,
                    fontSize = 12.sp
                )
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Text("Floodgate v2 (sin cuenta Java)", fontSize = 14.sp)
                    Switch(
                        checked = config.floodgateEnabled,
                        onCheckedChange = { onUpdateConfig(config.copy(floodgateEnabled = it)) }
                    )
                }
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.SpaceBetween,
                    verticalAlignment = Alignment.CenterVertically
                ) {
                    Text("ViaVersion automático", fontSize = 14.sp)
                    Switch(
                        checked = config.viaVersionEnabled,
                        onCheckedChange = { onUpdateConfig(config.copy(viaVersionEnabled = it)) }
                    )
                }
                Button(
                    onClick = {
                        val dir = EngineLaunchPlanner.createOrUpdateHybridProfile(context.filesDir, config)
                        onUpdateConfig(config.copy(activeLane = EngineLane.HYBRID))
                        hybridNotice = "Perfil Híbrido creado aparte en ${dir.name}"
                        ServerRuntimeState.appendLog(LogStream.STDOUT, "[Convertidor] $hybridNotice")
                    },
                    modifier = Modifier.fillMaxWidth()
                ) {
                    Text("Generar / Actualizar Carril Híbrido Aparte")
                }
                hybridNotice?.let {
                    Text(it, fontSize = 12.sp, color = Color(0xFF047857), fontWeight = FontWeight.Medium)
                }
            }
        }
    }
}
