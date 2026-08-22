package com.raylander.dock

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.content.pm.PackageManager
import android.app.AlertDialog
import android.graphics.Color
import android.os.Bundle
import android.text.InputType
import android.view.View
import android.view.WindowManager
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.Toast
import android.webkit.GeolocationPermissions
import android.webkit.JavascriptInterface
import android.webkit.WebChromeClient
import android.webkit.WebView
import android.webkit.WebViewClient

class MainActivity : Activity() {
    private lateinit var webView: WebView
    private val preferences by lazy { getSharedPreferences("dock", MODE_PRIVATE) }
    private var displayEnabled = true

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        window.attributes = window.attributes.apply {
            layoutInDisplayCutoutMode =
                WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES
        }
        enterImmersiveMode()
        if (checkSelfPermission(Manifest.permission.ACCESS_COARSE_LOCATION) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(arrayOf(Manifest.permission.ACCESS_COARSE_LOCATION), LOCATION_PERMISSION_REQUEST)
        }

        webView = WebView(this).apply {
            setBackgroundColor(Color.BLACK)
            settings.javaScriptEnabled = true
            settings.domStorageEnabled = true
            settings.mediaPlaybackRequiresUserGesture = false
            settings.setSupportZoom(false)
            isVerticalScrollBarEnabled = false
            isHorizontalScrollBarEnabled = false
            webViewClient = WebViewClient()
            webChromeClient = object : WebChromeClient() {
                override fun onGeolocationPermissionsShowPrompt(
                    origin: String,
                    callback: GeolocationPermissions.Callback,
                ) {
                    callback.invoke(
                        origin,
                        checkSelfPermission(Manifest.permission.ACCESS_COARSE_LOCATION) == PackageManager.PERMISSION_GRANTED,
                        false,
                    )
                }
            }
            addJavascriptInterface(ScreenBridge(this@MainActivity), "RaylanderAndroid")
            setOnLongClickListener {
                showServerDialog()
                true
            }
        }
        setContentView(webView)
        if (preferences.contains(KEY_HOST)) {
            loadDashboard()
        } else {
            showServerDialog(required = true)
        }
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) enterImmersiveMode()
    }

    override fun onResume() {
        super.onResume()
        enterImmersiveMode()
        setDockDisplay(displayEnabled)
    }

    @Deprecated("Deprecated in Java")
    override fun onBackPressed() {
        if (webView.canGoBack()) webView.goBack()
    }

    private fun loadDashboard() {
        val host = preferences.getString(KEY_HOST, DEFAULT_HOST).orEmpty().trim()
        val port = preferences.getInt(KEY_PORT, DEFAULT_PORT)
        webView.loadUrl("http://$host:$port/")
    }

    private fun showServerDialog(required: Boolean = false) {
        val hostInput = EditText(this).apply {
            hint = "Host IP"
            setSingleLine()
            inputType = InputType.TYPE_CLASS_TEXT
            setText(preferences.getString(KEY_HOST, DEFAULT_HOST))
        }
        val portInput = EditText(this).apply {
            hint = "Port"
            setSingleLine()
            inputType = InputType.TYPE_CLASS_NUMBER
            setText(preferences.getInt(KEY_PORT, DEFAULT_PORT).toString())
        }
        val fields = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            val padding = (24 * resources.displayMetrics.density).toInt()
            setPadding(padding, 0, padding, 0)
            addView(hostInput)
            addView(portInput)
        }
        val dialog = AlertDialog.Builder(this)
            .setTitle("Raylander server")
            .setMessage("Enter the computer's LAN address")
            .setView(fields)
            .setCancelable(!required)
            .setPositiveButton("Connect", null)
            .apply { if (!required) setNegativeButton("Cancel", null) }
            .create()
        dialog.setOnShowListener {
            dialog.getButton(AlertDialog.BUTTON_POSITIVE).setOnClickListener {
                val host = hostInput.text.toString().trim()
                    .removePrefix("http://").removePrefix("https://").trimEnd('/')
                val port = portInput.text.toString().toIntOrNull()
                if (host.isBlank() || port !in 1..65535) {
                    Toast.makeText(this, "Enter a valid host and port", Toast.LENGTH_SHORT).show()
                    return@setOnClickListener
                }
                preferences.edit().putString(KEY_HOST, host).putInt(KEY_PORT, port!!).apply()
                dialog.dismiss()
                loadDashboard()
            }
        }
        dialog.show()
    }

    private fun enterImmersiveMode() {
        @Suppress("DEPRECATION")
        window.decorView.systemUiVisibility =
            View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY or
            View.SYSTEM_UI_FLAG_FULLSCREEN or
            View.SYSTEM_UI_FLAG_HIDE_NAVIGATION or
            View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN or
            View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION or
            View.SYSTEM_UI_FLAG_LAYOUT_STABLE
    }

    private fun setDockDisplay(enabled: Boolean) {
        displayEnabled = enabled
        val attributes = window.attributes
        attributes.screenBrightness = if (enabled) WindowManager.LayoutParams.BRIGHTNESS_OVERRIDE_NONE else 0f
        window.attributes = attributes
        webView.visibility = if (enabled) View.VISIBLE else View.INVISIBLE
        window.decorView.setBackgroundColor(Color.BLACK)
    }

    private class ScreenBridge(private val activity: MainActivity) {
        @JavascriptInterface
        fun setDisplayEnabled(enabled: Boolean) {
            activity.runOnUiThread { activity.setDockDisplay(enabled) }
        }
    }

    companion object {
        private const val KEY_HOST = "host"
        private const val KEY_PORT = "port"
        private const val DEFAULT_HOST = "192.168.1.100"
        private const val DEFAULT_PORT = 3000
        private const val LOCATION_PERMISSION_REQUEST = 1
    }
}
