package dev.rustymap.app

import android.Manifest
import android.annotation.SuppressLint
import android.app.NativeActivity
import android.content.Intent
import android.content.pm.PackageManager
import android.location.Location
import android.location.LocationListener
import android.location.LocationManager
import android.location.LocationRequest
import android.os.Bundle
import android.view.HapticFeedbackConstants
import android.view.SurfaceView
import android.view.View
import android.view.ViewGroup
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : NativeActivity() {

    companion object {
        init {
            // Already loaded by NativeActivity; loading it from Java as well binds the
            // `external` functions below.
            System.loadLibrary("rusty_map")
        }

        /** System bar heights in dp; the app pads its overlays (see `src/mobile.rs`). */
        @JvmStatic
        external fun nativeSetInsets(top: Float, bottom: Float)

        /** A new fix; `accuracy` < 0 and a NaN `bearing` mean unknown (see `src/location.rs`). */
        @JvmStatic
        external fun nativeSetLocation(lat: Double, lon: Double, accuracy: Float, bearing: Float)

        /** Location permission refused, or location turned off in the system settings. */
        @JvmStatic
        external fun nativeLocationUnavailable()

        private const val LOCATION_REQUEST_CODE = 1001
        private const val UPDATE_INTERVAL_MS = 1000L
        /** A fix older than the last one by more than this is dropped. */
        private const val STALE_MS = 10_000L
    }

    /** Set by the app's location control; updates run only while this and the activity is resumed. */
    private var locationWanted = false
    private var listening = false
    private var lastFix: Location? = null

    private val locationManager by lazy { getSystemService(LocationManager::class.java) }

    private val locationListener = LocationListener { location -> onFix(location) }

    private fun findNativeSurfaceView(view: View): View? {
        if (view is SurfaceView) return view
        if (view is ViewGroup) {
            for (i in 0 until view.childCount) {
                val found = findNativeSurfaceView(view.getChildAt(i))
                if (found != null) return found
            }
        }
        return null
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        // Draw edge to edge on every version (enforced from API 35) with dark status bar icons
        // over the light map, and report the bars' sizes so the app pads itself.
        WindowCompat.setDecorFitsSystemWindows(window, false)
        WindowCompat.getInsetsController(window, window.decorView).apply {
            isAppearanceLightStatusBars = true
            isAppearanceLightNavigationBars = true
        }
        ViewCompat.setOnApplyWindowInsetsListener(window.decorView) { view, insets ->
            val density = resources.displayMetrics.density
            val bars = insets.getInsets(
                WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout()
            )
            nativeSetInsets(bars.top / density, bars.bottom / density)
            ViewCompat.onApplyWindowInsets(view, insets)
        }

        window.decorView.post {
            findNativeSurfaceView(window.decorView)?.apply {
                isFocusable = true
                isFocusableInTouchMode = true
                requestFocus()
            }
        }
    }

    override fun onResume() {
        super.onResume()
        if (locationWanted) startLocation()
    }

    override fun onPause() {
        super.onPause()
        stopLocation()
    }

    /** Called from Rust (`mobile::share`) on any thread: the system share sheet for `text`. */
    @Suppress("unused")
    fun share(subject: String, text: String) {
        runOnUiThread {
            val send = Intent(Intent.ACTION_SEND).apply {
                type = "text/plain"
                putExtra(Intent.EXTRA_SUBJECT, subject)
                putExtra(Intent.EXTRA_TEXT, text)
            }
            startActivity(Intent.createChooser(send, null))
        }
    }

    /** Called from Rust (`mobile::haptic`) on any thread; `kind` is `mobile::Haptic`. */
    @Suppress("unused")
    fun haptic(kind: Int) {
        val constant = when (kind) {
            1 -> HapticFeedbackConstants.TOGGLE_ON
            2 -> HapticFeedbackConstants.CONFIRM
            else -> HapticFeedbackConstants.VIRTUAL_KEY
        }
        runOnUiThread { window.decorView.performHapticFeedback(constant) }
    }

    /** Called from Rust (`location::set_tracking`) on any thread. */
    @Suppress("unused")
    fun setLocationTracking(enabled: Boolean) {
        runOnUiThread {
            locationWanted = enabled
            if (enabled) startLocation() else stopLocation()
        }
    }

    private fun hasLocationPermission(): Boolean =
        checkSelfPermission(Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED ||
            checkSelfPermission(Manifest.permission.ACCESS_COARSE_LOCATION) == PackageManager.PERMISSION_GRANTED

    @SuppressLint("MissingPermission")
    private fun startLocation() {
        if (listening) return
        if (!hasLocationPermission()) {
            requestPermissions(
                arrayOf(Manifest.permission.ACCESS_FINE_LOCATION, Manifest.permission.ACCESS_COARSE_LOCATION),
                LOCATION_REQUEST_CODE
            )
            return
        }
        val manager = locationManager
        if (!manager.isLocationEnabled) {
            locationWanted = false
            nativeLocationUnavailable()
            return
        }
        val providers = if (manager.hasProvider(LocationManager.FUSED_PROVIDER)) {
            listOf(LocationManager.FUSED_PROVIDER)
        } else {
            listOf(LocationManager.GPS_PROVIDER, LocationManager.NETWORK_PROVIDER)
                .filter { manager.isProviderEnabled(it) }
        }
        if (providers.isEmpty()) {
            locationWanted = false
            nativeLocationUnavailable()
            return
        }
        val request = LocationRequest.Builder(UPDATE_INTERVAL_MS)
            .setQuality(LocationRequest.QUALITY_HIGH_ACCURACY)
            .setMinUpdateIntervalMillis(UPDATE_INTERVAL_MS / 2)
            .build()
        // The last known fix first, so the dot shows before the first fresh one.
        providers.mapNotNull { manager.getLastKnownLocation(it) }
            .maxByOrNull { it.elapsedRealtimeNanos }
            ?.let { onFix(it) }
        for (provider in providers) {
            manager.requestLocationUpdates(provider, request, mainExecutor, locationListener)
        }
        listening = true
    }

    private fun stopLocation() {
        if (!listening) return
        locationManager.removeUpdates(locationListener)
        listening = false
    }

    // Several providers may feed the listener (GPS and network without the fused one): keep a
    // fix unless it is older than the last one or clearly worse while the last is still fresh.
    private fun onFix(location: Location) {
        val last = lastFix
        if (last != null) {
            val ageDifferenceMs = (location.elapsedRealtimeNanos - last.elapsedRealtimeNanos) / 1_000_000
            if (ageDifferenceMs < 0) return
            val worse = location.hasAccuracy() && last.hasAccuracy() &&
                location.accuracy > last.accuracy * 2 && location.accuracy > 50f
            if (worse && ageDifferenceMs < STALE_MS) return
        }
        lastFix = location
        val bearing = if (location.hasBearing() && location.hasSpeed() && location.speed > 0.5f) {
            location.bearing
        } else {
            Float.NaN
        }
        nativeSetLocation(
            location.latitude,
            location.longitude,
            if (location.hasAccuracy()) location.accuracy else -1f,
            bearing
        )
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode != LOCATION_REQUEST_CODE) return
        if (grantResults.any { it == PackageManager.PERMISSION_GRANTED }) {
            if (locationWanted) startLocation()
        } else {
            locationWanted = false
            nativeLocationUnavailable()
        }
    }

    // Back is handled in native code (Freya `on_global_key_down`, `NamedKey::BrowserBack`):
    // suppressing the default keeps the activity from finishing before it gets the key.
    @SuppressLint("GestureBackNavigation")
    @Deprecated("Deprecated in Java")
    override fun onBackPressed() {
    }

    override fun onDestroy() {
        super.onDestroy()
        // winit allows one event loop per process: the next launch must be a fresh process.
        if (isFinishing) android.os.Process.killProcess(android.os.Process.myPid())
    }
}
