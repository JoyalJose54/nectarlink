// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.webcam

import android.view.Surface

/** The view owns the Surface; the encoder owns only its temporary EGL window. */
internal data class WebcamPreviewTarget(val surface: Surface, val width: Int, val height: Int)
