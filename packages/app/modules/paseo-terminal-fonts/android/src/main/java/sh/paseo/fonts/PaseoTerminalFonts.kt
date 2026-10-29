package sh.paseo.fonts

import android.content.Context
import android.graphics.Paint
import android.graphics.Typeface
import android.graphics.fonts.Font
import android.graphics.fonts.FontFamily
import android.graphics.fonts.FontStyle
import android.util.Log
import com.facebook.react.common.assets.ReactFontManager

/** Registered before React starts, so first measurement and first paint share the same fonts. */
object PaseoTerminalFonts {
  const val FAMILY = "Paseo Terminal Mono"
  private const val ASSETS = "paseo-terminal-fonts/"

  fun install(context: Context) {
    try {
      val assets = context.assets
      val latin = FontFamily.Builder(Font.Builder(assets, ASSETS + "JetBrainsMono-Regular.ttf").build())
        .addFont(Font.Builder(assets, ASSETS + "JetBrainsMono-Bold.ttf").build())
        .addFont(Font.Builder(assets, ASSETS + "JetBrainsMono-Italic.ttf").build())
        .addFont(Font.Builder(assets, ASSETS + "JetBrainsMono-BoldItalic.ttf").build())
        .build()
      val chinese = FontFamily.Builder(Font.Builder(assets, ASSETS + "MapleMonoNormal-NF-CN-Regular.ttf").build()).build()
      val combined = Typeface.CustomFallbackBuilder(latin)
        .addCustomFallback(chinese)
        .setSystemFallback("monospace")
        .setStyle(FontStyle(400, FontStyle.FONT_SLANT_UPRIGHT))
        .build()
      val maple = Typeface.CustomFallbackBuilder(chinese)
        .addCustomFallback(latin)
        .setSystemFallback("monospace")
        .build()
      val manager = ReactFontManager.getInstance()
      manager.addCustomFont(FAMILY, combined)
      manager.addCustomFont("JetBrains Mono", combined)
      manager.addCustomFont("Maple Mono Normal NF CN", maple)
      val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = combined
        textSize = 14f
        // Match React Native's CustomStyleSpan when measuring the grid.
        isSubpixelText = true
        isLinearText = true
      }
      fun advance(text: String) = paint.getRunAdvance(text, 0, text.length, 0, text.length, false, text.length)
      Log.i("PaseoTerminalFonts", "Bundled fonts ready: W=${advance("W")} i=${advance("i")} space=${advance(" ")} CJK=${advance("中文") / 2}")
    } catch (error: Exception) {
      for (family in listOf(FAMILY, "JetBrains Mono", "Maple Mono Normal NF CN")) {
        ReactFontManager.getInstance().addCustomFont(family, Typeface.MONOSPACE)
      }
      Log.e("PaseoTerminalFonts", "Could not load bundled terminal fonts", error)
    }
  }
}
