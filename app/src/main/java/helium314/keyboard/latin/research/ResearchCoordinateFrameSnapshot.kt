// SPDX-License-Identifier: GPL-3.0-only
package helium314.keyboard.latin.research

import android.content.Context
import android.os.Build
import android.util.DisplayMetrics
import android.view.Surface
import android.view.View
import android.view.WindowManager
import helium314.keyboard.keyboard.KeyboardSwitcher

internal object ResearchCoordinateFrameSnapshot {
    private const val COORDINATE_SPACE = "keyboard_view_local_px"

    fun current(context: Context): CoordinateFrameSnapshot =
        fromView(context, KeyboardSwitcher.getInstance().mainKeyboardView)

    fun fromView(context: Context, keyboardView: View?): CoordinateFrameSnapshot {
        val display = displaySnapshot(context, keyboardView)
        if (keyboardView == null) {
            return CoordinateFrameSnapshot(
                available = false,
                unavailableReason = "keyboard_view_unavailable",
                display = display
            )
        }

        val locationOnScreen = IntArray(2)
        keyboardView.getLocationOnScreen(locationOnScreen)
        val left = locationOnScreen[0]
        val top = locationOnScreen[1]
        val width = keyboardView.width
        val height = keyboardView.height
        return CoordinateFrameSnapshot(
            available = true,
            unavailableReason = null,
            keyboardViewVisible = keyboardView.isShown,
            keyboardViewWidthPx = width,
            keyboardViewHeightPx = height,
            keyboardViewLeftOnScreenPx = left,
            keyboardViewTopOnScreenPx = top,
            keyboardViewRightOnScreenPx = left + width,
            keyboardViewBottomOnScreenPx = top + height,
            keyboardVisibleTopYScreenPx = if (keyboardView.isShown) top else null,
            keyboardVisibleHeightPx = if (keyboardView.isShown) height else 0,
            display = display
        )
    }

    private fun displaySnapshot(context: Context, keyboardView: View?): DisplaySnapshot {
        val appContext = context.applicationContext
        val windowManager = appContext.getSystemService(Context.WINDOW_SERVICE) as? WindowManager
        val display = keyboardView?.display ?: if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            runCatching { context.display }.getOrNull()
        } else {
            @Suppress("DEPRECATION")
            windowManager?.defaultDisplay
        }
        val metrics = DisplayMetrics()
        if (display != null) {
            @Suppress("DEPRECATION")
            display.getRealMetrics(metrics)
        } else {
            metrics.setTo(appContext.resources.displayMetrics)
        }
        return DisplaySnapshot(
            widthPx = metrics.widthPixels,
            heightPx = metrics.heightPixels,
            rotation = display?.rotation,
            rotationName = display?.rotation?.let(::rotationName)
        )
    }

    private fun rotationName(rotation: Int): String =
        when (rotation) {
            Surface.ROTATION_0 -> "rotation_0"
            Surface.ROTATION_90 -> "rotation_90"
            Surface.ROTATION_180 -> "rotation_180"
            Surface.ROTATION_270 -> "rotation_270"
            else -> "unknown"
        }

    data class CoordinateFrameSnapshot(
        val available: Boolean,
        val unavailableReason: String?,
        val keyboardViewVisible: Boolean? = null,
        val keyboardViewWidthPx: Int? = null,
        val keyboardViewHeightPx: Int? = null,
        val keyboardViewLeftOnScreenPx: Int? = null,
        val keyboardViewTopOnScreenPx: Int? = null,
        val keyboardViewRightOnScreenPx: Int? = null,
        val keyboardViewBottomOnScreenPx: Int? = null,
        val keyboardVisibleTopYScreenPx: Int? = null,
        val keyboardVisibleHeightPx: Int? = null,
        val display: DisplaySnapshot
    ) {
        fun fieldsForLocalPoint(xPx: Number? = null, yPx: Number? = null): Map<String, Any?> {
            val x = xPx?.toDouble()
            val y = yPx?.toDouble()
            val screenX = if (x != null && keyboardViewLeftOnScreenPx != null) {
                keyboardViewLeftOnScreenPx + x
            } else {
                null
            }
            val screenY = if (y != null && keyboardViewTopOnScreenPx != null) {
                keyboardViewTopOnScreenPx + y
            } else {
                null
            }
            return fields() + mapOf(
                "x_screen_px" to screenX,
                "y_screen_px" to screenY
            )
        }

        fun fields(): Map<String, Any?> =
            mapOf(
                "coordinate_space" to COORDINATE_SPACE,
                "coordinate_frame_available" to available,
                "coordinate_frame_unavailable_reason" to unavailableReason,
                "keyboard_view_visible" to keyboardViewVisible,
                "keyboard_view_width_px" to keyboardViewWidthPx,
                "keyboard_view_height_px" to keyboardViewHeightPx,
                "keyboard_view_left_screen_px" to keyboardViewLeftOnScreenPx,
                "keyboard_view_top_screen_px" to keyboardViewTopOnScreenPx,
                "keyboard_view_right_screen_px" to keyboardViewRightOnScreenPx,
                "keyboard_view_bottom_screen_px" to keyboardViewBottomOnScreenPx,
                "keyboard_visible_top_y_screen_px" to keyboardVisibleTopYScreenPx,
                "keyboard_visible_height_px" to keyboardVisibleHeightPx,
                "display_width_px" to display.widthPx,
                "display_height_px" to display.heightPx,
                "display_rotation" to display.rotation,
                "display_rotation_name" to display.rotationName
            )

    }

    data class DisplaySnapshot(
        val widthPx: Int,
        val heightPx: Int,
        val rotation: Int?,
        val rotationName: String?
    )
}
