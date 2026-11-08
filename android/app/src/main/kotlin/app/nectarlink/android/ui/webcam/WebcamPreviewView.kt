// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.ui.webcam

import android.content.Context
import android.graphics.Outline
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.ViewOutlineProvider
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.Shape
import androidx.compose.ui.graphics.asAndroidPath
import androidx.compose.ui.unit.Density
import androidx.compose.ui.unit.LayoutDirection
import app.nectarlink.android.webcam.WebcamPreviewTarget

/** Display sink only: destroying this surface never reconfigures the camera. */
internal class WebcamPreviewView(
    context: Context,
    private val onTargetChanged: (WebcamPreviewTarget?) -> Unit,
) : SurfaceView(context), SurfaceHolder.Callback {
    private var enabled = false
    private var surfaceWidth = 0
    private var surfaceHeight = 0

    init {
        holder.addCallback(this)
        clipToOutline = true
    }

    fun setPreviewVisible(visible: Boolean) {
        enabled = visible
        publishTarget()
    }

    private fun publishTarget() {
        onTargetChanged(if (enabled && holder.surface.isValid && surfaceWidth > 0 && surfaceHeight > 0) {
            WebcamPreviewTarget(holder.surface, surfaceWidth, surfaceHeight)
        } else null)
    }

    override fun surfaceCreated(holder: SurfaceHolder) {}

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        surfaceWidth = width
        surfaceHeight = height
        publishTarget()
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        surfaceWidth = 0
        surfaceHeight = 0
        onTargetChanged(null)
    }

    fun setPreviewShape(shape: Shape, density: Density, direction: LayoutDirection) {
        outlineProvider = object : ViewOutlineProvider() {
            override fun getOutline(view: View, outline: Outline) {
                when (val result = shape.createOutline(Size(view.width.toFloat(), view.height.toFloat()), direction, density)) {
                    is androidx.compose.ui.graphics.Outline.Rectangle -> outline.setRect(0, 0, view.width, view.height)
                    is androidx.compose.ui.graphics.Outline.Rounded -> {
                        val path = Path().apply { addRoundRect(result.roundRect) }.asAndroidPath()
                        @Suppress("DEPRECATION")
                        outline.setConvexPath(path)
                    }
                    is androidx.compose.ui.graphics.Outline.Generic -> {
                        @Suppress("DEPRECATION")
                        outline.setConvexPath(result.path.asAndroidPath())
                    }
                }
            }
        }
        invalidateOutline()
    }
}
