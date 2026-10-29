package sh.paseo.fonts

import android.graphics.Paint
import android.graphics.Typeface
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.facebook.react.common.assets.ReactFontManager
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class TerminalFontsTest {
  @Test fun registeredFontKeepsLatinSpacesAndChineseOnTheSameGrid() {
    val context = InstrumentationRegistry.getInstrumentation().targetContext
    PaseoTerminalFonts.install(context)
    for (style in listOf(Typeface.NORMAL, Typeface.BOLD, Typeface.ITALIC, Typeface.BOLD_ITALIC)) {
      val font = ReactFontManager.getInstance().getTypeface(PaseoTerminalFonts.FAMILY, style, context.assets)
      val paint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = font
        textSize = 28f
        isSubpixelText = true
        isLinearText = true
      }
      // Paint.measureText rounds the final width; layouts position glyphs by run advances.
      fun advance(text: String) = paint.getRunAdvance(text, 0, text.length, 0, text.length, false, text.length)
      val cell = advance("m")
      assertEquals("Bundled JetBrains Mono width for style $style", 16.8f, cell, 0.02f)
      for (text in listOf("W", "i", " ", "0")) {
        assertEquals("Single cell $text for style $style", cell, advance(text), 0.02f)
      }
      for (text in listOf("中", "文", "编", "程")) {
        assertTrue("Bundled Maple glyph $text", paint.hasGlyph(text))
        assertEquals("Two-cell Chinese $text for style $style", cell * 2, advance(text), 0.02f)
      }
      assertEquals("Mixed Latin, spaces and Chinese", cell * 10, advance("Wi  中文  "), 0.05f)
    }
  }
}
