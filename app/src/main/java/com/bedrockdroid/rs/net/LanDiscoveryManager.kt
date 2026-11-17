package com.bedrockdroid.rs.net

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.util.Log
import com.bedrockdroid.rs.model.LogStream
import com.bedrockdroid.rs.model.ServerRuntimeState
import java.net.Inet4Address
import java.net.Inet6Address
import java.net.InetAddress
import java.net.NetworkInterface

class LanDiscoveryManager(context: Context) {
    private val appContext = context.applicationContext
    private val nsdManager = appContext.getSystemService(Context.NSD_SERVICE) as NsdManager
    private var listener: NsdManager.RegistrationListener? = null

    private fun resolveLanIpAddress(): String {
        val interfaceCandidates = runCatching {
            val ifaces = NetworkInterface.getNetworkInterfaces()?.toList().orEmpty()
            val prioritized = mutableListOf<String>()
            val fallbackSiteLocal = mutableListOf<String>()

            for (iface in ifaces) {
                if (!iface.isUp || iface.isLoopback) continue
                val ifaceName = iface.name.lowercase()
                if (ifaceName.startsWith("tun") || ifaceName.startsWith("ppp") || ifaceName.startsWith("rmnet")) continue
                for (addr in iface.inetAddresses.toList()) {
                    if (addr is Inet4Address && !addr.isLoopbackAddress) {
                        val hostAddr = addr.hostAddress ?: continue
                        if (addr.isSiteLocalAddress) {
                            if (ifaceName.startsWith("wlan") || ifaceName.startsWith("eth") || ifaceName.startsWith("ap")) {
                                prioritized.add(hostAddr)
                            } else {
                                fallbackSiteLocal.add(hostAddr)
                            }
                        }
                    }
                }
            }
            prioritized.firstOrNull() ?: fallbackSiteLocal.firstOrNull()
        }.getOrNull()

        if (!interfaceCandidates.isNullOrBlank()) {
            return interfaceCandidates
        }

        val wifiIp = runCatching {
            val wifi = appContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager
            @Suppress("DEPRECATION")
            val rawIp = wifi?.connectionInfo?.ipAddress ?: 0
            if (rawIp != 0) {
                String.format(
                    "%d.%d.%d.%d",
                    rawIp and 0xff,
                    rawIp shr 8 and 0xff,
                    rawIp shr 16 and 0xff,
                    rawIp shr 24 and 0xff
                )
            } else {
                null
            }
        }.getOrNull()

        return wifiIp ?: "127.0.0.1"
    }

    private fun resolveLanIpv6Address(): String? {
        return runCatching {
            val ifaces = NetworkInterface.getNetworkInterfaces()?.toList().orEmpty()
            val prioritized = mutableListOf<String>()
            val fallback = mutableListOf<String>()

            for (iface in ifaces) {
                if (!iface.isUp || iface.isLoopback) continue
                val ifaceName = iface.name.lowercase()
                if (ifaceName.startsWith("tun") || ifaceName.startsWith("ppp") || ifaceName.startsWith("rmnet")) continue
                for (addr in iface.inetAddresses.toList()) {
                    if (addr is Inet6Address && !addr.isLoopbackAddress && !addr.isLinkLocalAddress && !addr.isMulticastAddress) {
                        val clean = addr.hostAddress?.substringBefore('%') ?: continue
                        if (ifaceName.startsWith("wlan") || ifaceName.startsWith("eth") || ifaceName.startsWith("ap")) {
                            prioritized.add(clean)
                        } else {
                            fallback.add(clean)
                        }
                    }
                }
            }
            prioritized.firstOrNull() ?: fallback.firstOrNull()
        }.getOrNull()
    }

    fun registerLanService(serverName: String, bedrockPort: Int) {
        unregister()
        val effectivePort = if (bedrockPort in 1..65535) bedrockPort else 19132
        val lanIpAddress = resolveLanIpAddress()
        val lanInetAddress = runCatching { InetAddress.getByName(lanIpAddress) }
            .getOrElse { InetAddress.getLoopbackAddress() }
        val lanIpv6Raw = resolveLanIpv6Address()
        val lanIpv6Address = lanIpv6Raw?.let { runCatching { InetAddress.getByName(it) }.getOrNull() }

        val serviceInfo = NsdServiceInfo().apply {
            serviceName = serverName
            serviceType = "_minecraft._udp"
            port = effectivePort
            // Convertir la IP string a InetAddress y asignarla explícitamente antes de registerService
            @Suppress("DEPRECATION")
            host = lanInetAddress
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
                hostAddresses = listOfNotNull(lanInetAddress, lanIpv6Address)
            }
            if (lanIpv6Address != null) {
                runCatching { setAttribute("ipv6", lanIpv6Address.hostAddress) }
            }
        }

        val aaaaDesc = lanIpv6Address?.hostAddress ?: "none"
        val startMsg = "[mDNS] Registrando _minecraft._udp: $serverName en ${lanInetAddress.hostAddress}:$effectivePort (AAAA=$aaaaDesc)"
        Log.i("BedrockDroidUI", startMsg)
        ServerRuntimeState.appendLog(LogStream.NET, startMsg, "BedrockDroidUI")

        val regListener = object : NsdManager.RegistrationListener {
            override fun onServiceRegistered(registeredInfo: NsdServiceInfo) {
                @Suppress("DEPRECATION")
                val boundHost = registeredInfo.host ?: lanInetAddress
                val boundPort = if (registeredInfo.port > 0) registeredInfo.port else effectivePort
                val msg = "[mDNS] Servicio registrado: ${registeredInfo.serviceName} en ${boundHost.hostAddress}:$boundPort (AAAA=$aaaaDesc)"
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
        nsdManager.registerService(serviceInfo, NsdManager.PROTOCOL_DNS_SD, regListener)
    }

    fun unregister() {
        listener?.let { runCatching { nsdManager.unregisterService(it) } }
        listener = null
    }
}
