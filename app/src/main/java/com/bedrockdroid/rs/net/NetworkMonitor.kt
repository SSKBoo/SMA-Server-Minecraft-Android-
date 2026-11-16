package com.bedrockdroid.rs.net

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest

class NetworkMonitor(context: Context) {
    private val cm = context.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
    private var callback: ConnectivityManager.NetworkCallback? = null

    fun startObserving(onNetworkChanged: () -> Unit) {
        stopObserving()
        val request = NetworkRequest.Builder()
            .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
            .build()

        val cb = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) = onNetworkChanged()
            override fun onLost(network: Network) = onNetworkChanged()
        }
        callback = cb
        cm.registerNetworkCallback(request, cb)
    }

    fun stopObserving() {
        callback?.let { runCatching { cm.unregisterNetworkCallback(it) } }
        callback = null
    }
}
