package com.bedrockdroid.rs.net

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo

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
            override fun onServiceRegistered(serviceInfo: NsdServiceInfo) = Unit
            override fun onRegistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) = Unit
            override fun onServiceUnregistered(serviceInfo: NsdServiceInfo) = Unit
            override fun onUnregistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) = Unit
        }
        listener = regListener
        nsdManager.registerService(info, NsdManager.PROTOCOL_DNS_SD, regListener)
    }

    fun unregister() {
        listener?.let { runCatching { nsdManager.unregisterService(it) } }
        listener = null
    }
}
