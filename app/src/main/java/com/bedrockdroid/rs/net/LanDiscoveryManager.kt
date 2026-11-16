package com.bedrockdroid.rs.net

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.util.Log
import com.bedrockdroid.rs.model.LogStream
import com.bedrockdroid.rs.model.ServerRuntimeState

class LanDiscoveryManager(context: Context) {
    private val nsdManager = context.getSystemService(Context.NSD_SERVICE) as NsdManager
    private var listener: NsdManager.RegistrationListener? = null

    fun registerLanService(serverName: String, udpPort: Int) {
        unregister()
        val info = NsdServiceInfo().apply {
            serviceName = serverName
            serviceType = "_minecraft._udp."
            port = udpPort
        }
        val regListener = object : NsdManager.RegistrationListener {
            override fun onServiceRegistered(serviceInfo: NsdServiceInfo) {
                val msg = "[mDNS] Servicio registrado: ${serviceInfo.serviceName} en ${serviceInfo.host}:${serviceInfo.port}"
                Log.i("BedrockDroidUI", msg)
                ServerRuntimeState.appendLog(LogStream.NET, msg, "BedrockDroidUI")
            }

            override fun onRegistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
                val msg = "[mDNS] Error al registrar ${serviceInfo.serviceName} (_minecraft._udp, errorCode=$errorCode)"
                Log.w("BedrockDroidUI", msg)
                ServerRuntimeState.appendLog(LogStream.WARN, msg, "BedrockDroidUI")
            }

            override fun onServiceUnregistered(serviceInfo: NsdServiceInfo) {
                val msg = "[mDNS] Servicio desregistrado: ${serviceInfo.serviceName}"
                Log.i("BedrockDroidUI", msg)
            }

            override fun onUnregistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
                val msg = "[mDNS] Error al desregistrar ${serviceInfo.serviceName} (errorCode=$errorCode)"
                Log.w("BedrockDroidUI", msg)
            }
        }
        listener = regListener
        nsdManager.registerService(info, NsdManager.PROTOCOL_DNS_SD, regListener)
    }

    fun unregister() {
        listener?.let { runCatching { nsdManager.unregisterService(it) } }
        listener = null
    }
}
