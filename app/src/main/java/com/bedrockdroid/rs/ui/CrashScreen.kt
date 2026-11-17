package com.bedrockdroid.rs.ui

import android.app.Activity
import android.content.Intent
import android.widget.Toast
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Share
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
import com.bedrockdroid.rs.crash.CrashHandler
import com.bedrockdroid.rs.crash.CrashLogFileManager

@Composable
fun CrashScreen(
    onCrashCleared: () -> Unit = {},
    modifier: Modifier = Modifier
) {
    val context = LocalContext.current
    val protectionEnabled by CrashHandler.protectionEnabled.collectAsState()
    val crashLogText by CrashHandler.crashLogContent.collectAsState()
    val hasPendingCrash by CrashHandler.hasPendingCrash.collectAsState()
    val crashFile = remember(context) { CrashLogFileManager.getCrashLogFile(context) }

    val vScroll = rememberScrollState()
    val hScroll = rememberScrollState()

    val exportLauncher = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.StartActivityForResult()
    ) { result ->
        if (result.resultCode == Activity.RESULT_OK) {
            val uri = result.data?.data ?: return@rememberLauncherForActivityResult
            CrashLogFileManager.exportCrashLogToUri(context, uri)
                .onSuccess { bytes ->
                    Toast.makeText(
                        context,
                        "Log de error exportado ($bytes bytes)",
                        Toast.LENGTH_LONG
                    ).show()
                }
                .onFailure { err ->
                    Toast.makeText(
                        context,
                        "Error al exportar: ${err.message}",
                        Toast.LENGTH_LONG
                    ).show()
                }
        }
    }

    LaunchedEffect(Unit) {
        CrashHandler.refreshState(context)
    }

    Column(
        modifier = modifier
            .fillMaxSize()
            .padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp)
    ) {
        Card(
            colors = CardDefaults.cardColors(
                containerColor = if (hasPendingCrash) Color(0xFFFEF2F2) else if (protectionEnabled) Color(0xFFFFFBEB) else Color.White
            ),
            border = androidx.compose.foundation.BorderStroke(
                1.dp,
                if (hasPendingCrash) Color(0xFFFCA5A5) else if (protectionEnabled) Color(0xFFFCD34D) else Color(0xFFE2E8F0)
            )
        ) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(12.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp)
            ) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(8.dp)
                ) {
                    Icon(
                        imageVector = Icons.Default.Warning,
                        contentDescription = null,
                        tint = if (hasPendingCrash) Color(0xFFDC2626) else if (protectionEnabled) Color(0xFFD97706) else Color(0xFF64748B)
                    )
                    Text(
                        text = when {
                            hasPendingCrash -> "Registro de Crash / Panic Detectado"
                            protectionEnabled -> "Protección ACTIVA (Solo Pruebas)"
                            else -> "Protección Desactivada (Uso Normal)"
                        },
                        fontWeight = FontWeight.Bold,
                        fontSize = 14.sp,
                        color = if (hasPendingCrash) Color(0xFF991B1B) else Color(0xFF0F172A)
                    )
                }
                Text(
                    text = if (protectionEnabled) {
                        "Modo pruebas activo: catch_unwind, set_hook y Kotlin guardan los errores en ${crashFile.absolutePath}"
                    } else {
                        "Desactivada para no guardar todo siempre en disco. Pulsa el botón solo cuando quieras hacer pruebas."
                    },
                    fontSize = 11.sp,
                    color = Color(0xFF475569)
                )

                Button(
                    onClick = {
                        val next = !protectionEnabled
                        CrashHandler.setProtectionEnabled(context, next)
                        Toast.makeText(
                            context,
                            if (next) {
                                "Protección de pruebas ACTIVADA: capturando Rust Panics en crash_log.txt"
                            } else {
                                "Protección de pruebas DESACTIVADA: modo normal sin guardado"
                            },
                            Toast.LENGTH_SHORT
                        ).show()
                    },
                    modifier = Modifier.fillMaxWidth(),
                    colors = ButtonDefaults.buttonColors(
                        containerColor = if (protectionEnabled) Color(0xFFB45309) else Color(0xFF047857)
                    )
                ) {
                    Text(
                        text = if (protectionEnabled) {
                            "Desactivar Protección (Solo Pruebas ACTIVA)"
                        } else {
                            "Activar Protección (Solo Pruebas)"
                        },
                        fontSize = 12.sp,
                        fontWeight = FontWeight.SemiBold
                    )
                }

                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp)
                ) {
                    Button(
                        onClick = {
                            val intent = Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
                                addCategory(Intent.CATEGORY_OPENABLE)
                                type = "text/plain"
                                putExtra(Intent.EXTRA_TITLE, "BedrockDroid_Crash_Log.txt")
                            }
                            exportLauncher.launch(intent)
                        },
                        modifier = Modifier.weight(1f),
                        colors = ButtonDefaults.buttonColors(containerColor = Color(0xFF0F172A))
                    ) {
                        Icon(Icons.Default.Share, contentDescription = null, modifier = Modifier.size(16.dp))
                        Spacer(Modifier.width(6.dp))
                        Text("Exportar Log de Error", fontSize = 12.sp)
                    }

                    OutlinedButton(
                        onClick = {
                            CrashHandler.clearLogs(context)
                            onCrashCleared()
                            Toast.makeText(context, "crash_log.txt limpiado", Toast.LENGTH_SHORT).show()
                        },
                        enabled = hasPendingCrash
                    ) {
                        Icon(Icons.Default.Delete, contentDescription = null, modifier = Modifier.size(16.dp))
                        Spacer(Modifier.width(4.dp))
                        Text("Limpiar Log", fontSize = 12.sp)
                    }
                }
            }
        }

        Box(
            modifier = Modifier
                .weight(1f)
                .fillMaxWidth()
                .clip(RoundedCornerShape(12.dp))
                .background(Color(0xFF090D16))
                .border(1.dp, Color(0xFF1E293B), RoundedCornerShape(12.dp))
                .padding(12.dp)
        ) {
            SelectionContainer {
                Text(
                    text = crashLogText.ifBlank {
                        "No se han registrado crashes ni pánicos nativos en crash_log.txt.\n\nLa protección inicia DESACTIVADA para no guardar todo continuamente. Activa el botón "Activar Protección (Solo Pruebas)" antes de probar la conexión cuando quieras capturar un Rust Panic en crash_log.txt."
                    },
                    fontFamily = FontFamily.Monospace,
                    fontSize = 11.sp,
                    lineHeight = 16.sp,
                    color = if (hasPendingCrash) Color(0xFFFCA5A5) else Color(0xFFCBD5E1),
                    softWrap = false,
                    modifier = Modifier
                        .fillMaxSize()
                        .verticalScroll(vScroll)
                        .horizontalScroll(hScroll)
                )
            }
        }
    }
}
