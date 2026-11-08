// SPDX-License-Identifier: GPL-3.0-or-later
package app.nectarlink.android.ui.theme

import android.graphics.RenderEffect
import android.graphics.RuntimeShader
import android.graphics.Shader
import android.os.Build
import androidx.annotation.RequiresApi
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Stable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.drawWithCache
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.RoundRect
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.asComposeRenderEffect
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.clipPath
import androidx.compose.ui.graphics.drawscope.translate
import androidx.compose.ui.graphics.layer.GraphicsLayer
import androidx.compose.ui.graphics.layer.drawLayer
import androidx.compose.ui.graphics.rememberGraphicsLayer
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.positionInRoot
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

/**
 * The content that floating glass controls sit over. The source records what it draws
 * into [layer]; glass surfaces redraw that recording, bent at their edges.
 */
@Stable
class GlassBackdrop internal constructor(internal val layer: GraphicsLayer) {
    internal var origin by mutableStateOf(Offset.Zero)
    internal var recorded by mutableStateOf(false)
}

@Composable
fun rememberGlassBackdrop(): GlassBackdrop {
    val layer = rememberGraphicsLayer()
    return remember(layer) { GlassBackdrop(layer) }
}

/** Marks the content that [liquidGlass] surfaces refract. Drawing is unchanged. */
fun Modifier.glassBackdropSource(backdrop: GlassBackdrop, enabled: Boolean): Modifier =
    if (!enabled || Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) {
        this
    } else {
        this
            .onGloballyPositioned { backdrop.origin = it.positionInRoot() }
            .drawWithContent {
                backdrop.layer.record { this@drawWithContent.drawContent() }
                drawLayer(backdrop.layer)
                backdrop.recorded = true
            }
    }

/**
 * A floating Liquid Glass surface: the backdrop behind it, lightly frosted and bent
 * toward the rim like a lens, with a specular edge and a tint that keeps text readable.
 * Falls back to [fallbackColor] where the shader isn't available or [enabled] is false.
 */
@Composable
fun Modifier.liquidGlass(
    backdrop: GlassBackdrop?,
    enabled: Boolean,
    cornerRadius: Dp,
    tint: Color,
    fallbackColor: Color,
    rimColor: Color,
    dark: Boolean,
): Modifier {
    val origin = remember { mutableStateOf(Offset.Zero) }
    return this
        .onGloballyPositioned { origin.value = it.positionInRoot() }
        .drawWithCache {
            val r = cornerRadius.toPx().coerceAtMost(size.minDimension / 2f)
            val cr = CornerRadius(r, r)
            val path = Path().apply { addRoundRect(RoundRect(0f, 0f, size.width, size.height, cr)) }
            val stroke = Stroke(width = 1.dp.toPx())
            val glassLayer = if (enabled && backdrop != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                obtainGraphicsLayer().also {
                    it.renderEffect = Lens.effect(
                        width = size.width,
                        height = size.height,
                        radius = r,
                        band = (r * 0.9f).coerceAtLeast(12.dp.toPx()),
                        bend = 9.dp.toPx(),
                        frost = 2.dp.toPx(),
                    ).asComposeRenderEffect()
                }
            } else {
                null
            }
            // Light from the top left: the rim catches it there and fades toward the far corner.
            val rim = Brush.linearGradient(
                0f to rimColor.copy(alpha = if (dark) 0.42f else 0.75f),
                0.45f to rimColor.copy(alpha = if (dark) 0.10f else 0.18f),
                1f to rimColor.copy(alpha = if (dark) 0.22f else 0.35f),
                start = Offset.Zero,
                end = Offset(size.width, size.height),
            )
            onDrawBehind {
                if (glassLayer != null && backdrop != null && backdrop.recorded) {
                    val dx = backdrop.origin.x - origin.value.x
                    val dy = backdrop.origin.y - origin.value.y
                    glassLayer.record {
                        translate(dx, dy) { drawLayer(backdrop.layer) }
                    }
                    clipPath(path) { drawLayer(glassLayer) }
                    drawPath(path, tint)
                } else {
                    drawPath(path, fallbackColor)
                }
                drawPath(path, rim, style = stroke)
            }
        }
}

@RequiresApi(Build.VERSION_CODES.TIRAMISU)
private object Lens {
    // Signed distance to the rounded rectangle gives each pixel its distance from the rim
    // and the rim's outward normal. Inside a band along the rim the backdrop is sampled
    // further in, so it appears magnified and bent the way thick glass bends light, and a
    // specular term brightens the rim where it faces the light.
    private const val AGSL = """
        uniform shader content;
        uniform float2 size;
        uniform float radius;
        uniform float band;
        uniform float bend;

        half4 main(float2 p) {
            float2 c = size * 0.5;
            float2 q = abs(p - c) - (c - radius);
            float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
            float edge = clamp(1.0 + d / band, 0.0, 1.0);
            float2 outward = sign(p - c) * max(q, 0.0);
            float len = length(outward);
            float2 n = len > 0.0001 ? outward / len : float2(0.0, 0.0);
            float shape = edge * edge * edge;
            half4 col = content.eval(p - n * shape * bend);
            float light = clamp(dot(n, normalize(float2(-0.55, -0.84))), 0.0, 1.0);
            col.rgb += half3(pow(edge, 6.0) * light * 0.22);
            return col;
        }
    """

    fun effect(width: Float, height: Float, radius: Float, band: Float, bend: Float, frost: Float): RenderEffect {
        val shader = RuntimeShader(AGSL).apply {
            setFloatUniform("size", width, height)
            setFloatUniform("radius", radius)
            setFloatUniform("band", band)
            setFloatUniform("bend", bend)
        }
        val lens = RenderEffect.createRuntimeShaderEffect(shader, "content")
        val blur = RenderEffect.createBlurEffect(frost, frost, Shader.TileMode.CLAMP)
        return RenderEffect.createChainEffect(lens, blur)
    }
}
