package com.bedrockdroid.rs

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Build
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.core.content.ContextCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsControllerCompat
import com.bedrockdroid.rs.engine.EngineLaunchPlanner
import com.bedrockdroid.rs.model.HardwareScanReport
import com.bedrockdroid.rs.model.LauncherPrefs
import com.bedrockdroid.rs.model.LogBuffer
import com.bedrockdroid.rs.model.LogLevel
import com.bedrockdroid.rs.model.NetworkProbeReport
import com.bedrockdroid.rs.model.ServerRuntimeState
import com.bedrockdroid.rs.service.ServerForegroundService
import com.bedrockdroid.rs.ui.BedrockDroidTheme
import com.bedrockdroid.rs.ui.ConsoleScreen
import com.bedrockdroid.rs.ui.EnginesScreen
import com.bedrockdroid.rs.ui.SettingsScreen
import com.bedrockdroid.rs.ui.SurvivalNetworkScreen
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File

private enum class AppTab(val label: String) {
    CONSOLE("Consola"),
    ENGINES("Motores"),
    SURVIVAL("24/7 y Red"),
    SETTINGS("Ajustes")
}

class MainActivity : ComponentActivity() {
    @OptIn(ExperimentalMaterial3Api::class)
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        runCatching {
            WindowCompat.setDecorFitsSystemWindows(window, false)
            WindowInsetsControllerCompat(window, window.decorView).apply {
                isAppearanceLightStatusBars = true
                isAppearanceLightNavigationBars = true
            }
        }

        setContent {
            BedrockDroidTheme {
                val scope = rememberCoroutineScope()
                var currentTab by remember { mutableStateOf(AppTab.CONSOLE) }
                var config by remember {
                    mutableStateOf(
                        runCatching { LauncherPrefs.load(this@MainActivity) }
                            .getOrElse { LauncherPrefs.defaultConfig() }
                    )
                }
                val isRunning by ServerRuntimeState.isRunning.collectAsState()
                val logs by ServerRuntimeState.logs.collectAsState()
                val hardwareReport by ServerRuntimeState.hardwareReport.collectAsState()
                val networkReport by ServerRuntimeState.networkReport.collectAsState()
                var latestCrashFile by remember { mutableStateOf<File?>(null) }
                var latestCrashText by remember { mutableStateOf<String?>(null) }

                LaunchedEffect(Unit) {
                    val found = withContext(Dispatchers.IO) {
                        val file = LogBuffer.readLatestCrashReport(this@MainActivity)
                        val text = file?.let { runCatching { it.readText() }.getOrNull() }
                        file to text
                    }
                    val (crashFile, crashBody) = found
                    latestCrashFile = crashFile
                    latestCrashText = crashBody
                    if (crashFile != null && crashBody != null) {
                        LogBuffer.appendLevel(
                            LogLevel.ERROR,
                            "CrashRecovery",
                            "Crash previo registrado en ${crashFile.name}:\n${crashBody.take(1500)}"
                        )
                    }

                    // Escaneo inicial de hardware y red en segundo plano para actualizar StatusCard al instante
                    withContext(Dispatchers.IO) {
                        runCatching { EngineLaunchPlanner.scanDeviceHardware(this@MainActivity) }
                        runCatching { EngineLaunchPlanner.probeNetwork(config) }
                    }
                }

                val updateConfig: (com.bedrockdroid.rs.model.AndroidLauncherConfig) -> Unit = { next ->
                    if (isRunning && next.activeLane != config.activeLane) {
                        android.widget.Toast.makeText(
                            this@MainActivity,
                            "Detén el servidor antes de cambiar de perfil",
                            android.widget.Toast.LENGTH_SHORT
                        ).show()
                    } else {
                        config = next
                        runCatching { LauncherPrefs.save(this@MainActivity, next) }
                    }
                }

                Scaffold(
                    containerColor = Color(0xFFF8FAFC),
                    topBar = {
                        TopAppBar(
                            title = {
                                Text(
                                    "BedrockDroid RS · ARM64",
                                    fontWeight = FontWeight.Bold
                                )
                            },
                            colors = TopAppBarDefaults.topAppBarColors(
                                containerColor = Color.White,
                                titleContentColor = Color(0xFF0F172A)
                            )
                        )
                    },
                    bottomBar = {
                        NavigationBar(containerColor = Color.White) {
                            NavigationBarItem(
                                selected = currentTab == AppTab.CONSOLE,
                                onClick = { currentTab = AppTab.CONSOLE },
                                icon = { Icon(Icons.Default.Home, contentDescription = null) },
                                label = { Text("Consola") }
                            )
                            NavigationBarItem(
                                selected = currentTab == AppTab.ENGINES,
                                onClick = { currentTab = AppTab.ENGINES },
                                icon = { Icon(Icons.Default.Build, contentDescription = null) },
                                label = { Text("Motores") }
                            )
                            NavigationBarItem(
                                selected = currentTab == AppTab.SURVIVAL,
                                onClick = { currentTab = AppTab.SURVIVAL },
                                icon = { Icon(Icons.Default.CheckCircle, contentDescription = null) },
                                label = { Text("24/7 y Red") }
                            )
                            NavigationBarItem(
                                selected = currentTab == AppTab.SETTINGS,
                                onClick = { currentTab = AppTab.SETTINGS },
                                icon = { Icon(Icons.Default.Settings, contentDescription = null) },
                                label = { Text("Ajustes") }
                            )
                        }
                    }
                ) { innerPadding ->
                    val contentModifier = Modifier
                        .padding(innerPadding)
                        .consumeWindowInsets(innerPadding)
                        .imePadding()
                    when (currentTab) {
                        AppTab.CONSOLE -> ConsoleScreen(
                            config = config,
                            isRunning = isRunning,
                            logs = logs,
                            hardwareReport = hardwareReport,
                            networkReport = networkReport,
                            onSelectLane = { lane -> updateConfig(config.copy(activeLane = lane)) },
                            onStartOrStop = {
                                if (isRunning) {
                                    stopService(Intent(this@MainActivity, ServerForegroundService::class.java))
                                } else {
                                    ContextCompat.startForegroundService(
                                        this@MainActivity,
                                        Intent(this@MainActivity, ServerForegroundService::class.java)
                                    )
                                }
                            },
                            onRunCommand = { cmd ->
                                scope.launch {
                                    withContext(Dispatchers.IO) {
                                        EngineLaunchPlanner.executeCliCommand(
                                            this@MainActivity,
                                            cmd,
                                            config
                                        ) { next ->
                                            scope.launch(Dispatchers.Main) {
                                                updateConfig(next)
                                            }
                                        }
                                    }
                                }
                            },
                            onClearLogs = { ServerRuntimeState.clearLogs() },
                            latestCrashFileName = latestCrashFile?.name,
                            latestCrashText = latestCrashText,
                            onShareCrash = {
                                latestCrashFile?.let { file ->
                                    runCatching {
                                        LogBuffer.shareFileWithChooser(
                                            context = this@MainActivity,
                                            file = file,
                                            subject = "BedrockDroid RS Crash Report (${file.name})",
                                            chooserTitle = "Compartir reporte de crash"
                                        )
                                    }
                                }
                            },
                            onDismissCrash = {
                                latestCrashFile = null
                                latestCrashText = null
                                LogBuffer.clearCrashReports(this@MainActivity)
                            },
                            modifier = contentModifier
                        )

                        AppTab.ENGINES -> EnginesScreen(
                            config = config,
                            onUpdateConfig = updateConfig,
                            modifier = contentModifier
                        )

                        AppTab.SURVIVAL -> SurvivalNetworkScreen(
                            config = config,
                            isRunning = isRunning,
                            hardwareReport = hardwareReport,
                            networkReport = networkReport,
                            onScanComplete = { hw, applyOptimal ->
                                ServerRuntimeState.updateHardwareReport(hw)
                                if (applyOptimal) {
                                    updateConfig(
                                        config.copy(
                                            jvmXmsMb = hw.recommendedXmsMb,
                                            jvmXmxMb = hw.recommendedXmxMb,
                                            wanMtuBytes = hw.recommendedMtu
                                        )
                                    )
                                }
                            },
                            onConfirmOemRule = { confirmed ->
                                updateConfig(config.copy(oemAutostartConfirmed = confirmed))
                            },
                            onNetworkProbeComplete = { ServerRuntimeState.updateNetworkReport(it) },
                            modifier = contentModifier
                        )

                        AppTab.SETTINGS -> SettingsScreen(
                            config = config,
                            onUpdateConfig = updateConfig,
                            modifier = contentModifier
                        )
                    }
                }
            }
        }
    }
}
