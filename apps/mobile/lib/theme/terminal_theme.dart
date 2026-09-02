import 'package:flutter/material.dart';

import 'app_theme.dart';

class TerminalPaintColors {
  const TerminalPaintColors({
    required this.fg,
    required this.bg,
    required this.drawBackground,
  });

  final Color fg;
  final Color bg;
  final bool drawBackground;
}

class TerminalTheme {
  // Resolved cell colors are a pure function of (fg, bg, inverse) plus the
  // compile-time AppColors. The painter calls this once per visible cell on
  // every repaint, and each call runs several computeLuminance() passes. A
  // terminal screen only has a handful of distinct color combos, so memoizing
  // by a compact value key removes nearly all of that per-cell luminance work.
  static final Map<String, TerminalPaintColors> _cellColorCache = {};

  /// Palette signature. The resolved colors depend on the active theme, so it
  /// belongs in the cache key — otherwise a theme switch keeps serving cells
  /// painted for the previous palette.
  static String get _paletteKey =>
      '${CoduxTheme.isLight ? 'l' : 'd'}${CoduxTheme.terminalMono ? 'm' : ''}';

  static String _colorKey(Map<String, dynamic> value) {
    final kind = '${value['kind'] ?? ''}';
    return switch (kind) {
      'rgb' => 'r${value['r']},${value['g']},${value['b']}',
      'indexed' => 'i${value['index']}',
      'named' => 'n${value['name']}',
      _ => kind,
    };
  }

  static TerminalPaintColors resolveCellColors({
    required Map<String, dynamic> fg,
    required Map<String, dynamic> bg,
    required bool inverse,
    bool bold = false,
    bool dim = false,
  }) {
    final cacheKey =
        '$_paletteKey|${_colorKey(fg)}|${_colorKey(bg)}|$inverse|$bold|$dim';
    final cached = _cellColorCache[cacheKey];
    if (cached != null) return cached;
    final resolved = _resolveCellColors(
      fg: fg,
      bg: bg,
      inverse: inverse,
      bold: bold,
      dim: dim,
    );
    if (_cellColorCache.length > 512) _cellColorCache.clear();
    _cellColorCache[cacheKey] = resolved;
    return resolved;
  }

  /// SGR 58 underline color; falls back to the resolved text foreground.
  static Color resolveUnderlineColor(
    Map<String, dynamic>? value,
    Color fallback,
  ) {
    if (value == null) return fallback;
    return _resolveScreenColor(value, fallback).color;
  }

  static TerminalPaintColors _resolveCellColors({
    required Map<String, dynamic> fg,
    required Map<String, dynamic> bg,
    required bool inverse,
    required bool bold,
    required bool dim,
  }) {
    final foreground = _resolveScreenColor(fg, AppColors.terminalText);
    var background = _resolveScreenColor(bg, AppColors.terminalBg);
    var fgColor = foreground.color;
    var bgColor = background.color;
    var drawBackground = !background.isDefault && bgColor != AppColors.terminalBg;

    if (background.isClashingSurface) {
      bgColor = AppColors.terminalElevated;
      drawBackground = true;
      background = background.copyWith(color: bgColor);
    }

    if (inverse) {
      var inverseFg = background.color;
      if (bold) {
        inverseFg = _boldForeground(bg, inverseFg);
      }
      inverseFg = _ensureReadable(inverseFg, foreground.color);
      if (dim) {
        inverseFg = _dimColor(inverseFg, foreground.color);
      }
      return _paint(
        fg: inverseFg,
        bg: foreground.color,
        drawBackground: true,
        inverted: true,
      );
    }

    if (bold) {
      fgColor = _boldForeground(fg, fgColor);
    }
    if (!foreground.isRgb) {
      fgColor = _ensureReadable(fgColor, bgColor);
    } else if (CoduxTheme.isLight) {
      // Hosts emit rgb tuned for a dark terminal; on a light canvas a pastel
      // straight from the host is unreadable. Blending toward black keeps the
      // luminance ordering, so gradients stay distinguishable.
      fgColor = _darkenForContrast(fgColor, bgColor);
    }
    if (dim) {
      fgColor = _dimColor(fgColor, bgColor);
    }

    return _paint(fg: fgColor, bg: bgColor, drawBackground: drawBackground);
  }

  /// Final gate. In mono (e-ink) mode the resolved colors collapse onto a gray
  /// ramp: the panel renders the tuned hues as near-identical mid grays, so hue
  /// distinctions never reach the painter and contrast is spent on legibility.
  static TerminalPaintColors _paint({
    required Color fg,
    required Color bg,
    required bool drawBackground,
    bool inverted = false,
  }) {
    if (!CoduxTheme.terminalMono) {
      return TerminalPaintColors(fg: fg, bg: bg, drawBackground: drawBackground);
    }
    if (inverted) {
      // A true highlight block: solid ink behind canvas-colored text.
      return TerminalPaintColors(
        fg: AppColors.terminalBg,
        bg: AppColors.terminalText,
        drawBackground: true,
      );
    }
    if (drawBackground) {
      return TerminalPaintColors(
        fg: AppColors.terminalText,
        bg: _monoBand(),
        drawBackground: true,
      );
    }
    return TerminalPaintColors(
      fg: _monoInk(fg, AppColors.terminalBg),
      bg: bg,
      drawBackground: false,
    );
  }
}

class _ResolvedScreenColor {
  const _ResolvedScreenColor({
    required this.color,
    required this.isDefault,
    required this.isRgb,
    required this.isClashingSurface,
  });

  final Color color;
  final bool isDefault;
  final bool isRgb;
  final bool isClashingSurface;

  _ResolvedScreenColor copyWith({Color? color}) {
    final nextColor = color ?? this.color;
    return _ResolvedScreenColor(
      color: nextColor,
      isDefault: isDefault,
      isRgb: isRgb,
      isClashingSurface: _isClashingSurface(nextColor),
    );
  }
}

_ResolvedScreenColor _resolveScreenColor(
  Map<String, dynamic> value,
  Color fallback,
) {
  final kind = '${value['kind'] ?? ''}';
  final color = switch (kind) {
    'rgb' => Color.fromARGB(
      255,
      _channel(value['r']),
      _channel(value['g']),
      _channel(value['b']),
    ),
    'indexed' => _ansiIndexedColor(value['index']),
    'named' => _namedColor('${value['name'] ?? ''}', fallback),
    _ => fallback,
  };
  return _ResolvedScreenColor(
    color: color,
    isDefault: kind.isEmpty || kind == 'default',
    isRgb: kind == 'rgb',
    isClashingSurface: kind != 'default' && _isClashingSurface(color),
  );
}

int _channel(Object? value) {
  if (value is num) return value.toInt().clamp(0, 255);
  return int.tryParse('${value ?? ''}')?.clamp(0, 255) ?? 0;
}

/// ANSI 16 for a dark canvas: soft, slightly desaturated hues.
const _darkAnsi = [
  Color(0xFF0D1117),
  Color(0xFFFF6B6B),
  Color(0xFF69DB7C),
  Color(0xFFFFD43B),
  Color(0xFF74C0FC),
  Color(0xFFE599F7),
  Color(0xFF66D9E8),
  Color(0xFFE6EDF3),
  Color(0xFF6E7681),
  Color(0xFFFF8787),
  Color(0xFF8CE99A),
  Color(0xFFFFE066),
  Color(0xFFA5D8FF),
  Color(0xFFF3B4FF),
  Color(0xFF99E9F2),
  Color(0xFFF8F9FA),
];

/// ANSI 16 for a light canvas: the same slots darkened so each clears ~4.5:1 on
/// white. Yellow has no readable light-canvas form, so it lands on brown.
const _lightAnsi = [
  Color(0xFF24292F),
  Color(0xFFCF222E),
  Color(0xFF116329),
  Color(0xFF7D4E00),
  Color(0xFF0969DA),
  Color(0xFF8250DF),
  Color(0xFF1B7C83),
  Color(0xFF6E7781),
  Color(0xFF57606A),
  Color(0xFFA40E26),
  Color(0xFF1A7F37),
  Color(0xFF9A6700),
  Color(0xFF0550AE),
  Color(0xFF6639BA),
  Color(0xFF106068),
  Color(0xFF24292F),
];

List<Color> get _ansiPalette => CoduxTheme.isLight ? _lightAnsi : _darkAnsi;

Color _ansiIndexedColor(Object? value) {
  final index = value is num ? value.toInt() : int.tryParse('${value ?? ''}');
  if (index == null) return AppColors.terminalText;
  final basic = _ansiPalette;
  if (index < basic.length) return basic[index.clamp(0, basic.length - 1)];
  if (index >= 16 && index <= 231) {
    final cube = index - 16;
    final r = cube ~/ 36;
    final g = (cube % 36) ~/ 6;
    final b = cube % 6;
    int channel(int value) => value == 0 ? 0 : 55 + value * 40;
    return Color.fromARGB(255, channel(r), channel(g), channel(b));
  }
  if (index >= 232 && index <= 255) {
    final value = 8 + (index - 232) * 10;
    return Color.fromARGB(255, value, value, value);
  }
  return AppColors.terminalText;
}

Color _boldForeground(Map<String, dynamic> value, Color fallback) {
  if ('${value['kind'] ?? ''}' != 'indexed') return fallback;
  final index = value['index'] is num
      ? (value['index'] as num).toInt()
      : int.tryParse('${value['index'] ?? ''}');
  if (index == null || index < 0 || index >= 8) return fallback;
  return _ansiIndexedColor(index + 8);
}

/// Named slots resolve through the same palette as the indexed ones, so the two
/// spellings of "red" can never drift apart per theme.
Color _namedColor(String name, Color fallback) {
  if (name == 'Background') return AppColors.terminalBg;
  final index = switch (name) {
    'Black' || 'DimBlack' => 0,
    'Red' || 'DimRed' => 1,
    'Green' || 'DimGreen' => 2,
    'Yellow' || 'DimYellow' => 3,
    'Blue' || 'DimBlue' => 4,
    'Magenta' || 'DimMagenta' => 5,
    'Cyan' || 'DimCyan' => 6,
    'White' || 'DimWhite' => 7,
    'BrightBlack' => 8,
    'BrightRed' => 9,
    'BrightGreen' => 10,
    'BrightYellow' => 11,
    'BrightBlue' => 12,
    'BrightMagenta' => 13,
    'BrightCyan' => 14,
    'BrightWhite' || 'BrightForeground' => 15,
    _ => null,
  };
  if (index == null) return fallback;
  return _ansiPalette[index];
}

Color _blendToward(Color from, Color to, double amount) {
  int channel(double a, double b) {
    final value = a * 255 * (1 - amount) + b * 255 * amount;
    return value.round().clamp(0, 255);
  }

  return Color.fromARGB(
    255,
    channel(from.r, to.r),
    channel(from.g, to.g),
    channel(from.b, to.b),
  );
}

Color _dimColor(Color fg, Color bg) => _blendToward(fg, bg, 0.4);

Color _darkenForContrast(Color fg, Color bg) {
  var result = fg;
  for (var i = 0; i < 4 && _contrastRatio(result, bg) < 3.0; i++) {
    result = _blendToward(result, const Color(0xFF000000), 0.35);
  }
  return result;
}

/// Mono (e-ink) ink ramp, ordered strongest first. Three widely spaced steps:
/// finer gradations dither on e-ink and read as the same shade.
const _monoLightRamp = [
  Color(0xFF000000),
  Color(0xFF3D3D3D),
  Color(0xFF6B6B6B),
];
const _monoDarkRamp = [
  Color(0xFFFFFFFF),
  Color(0xFFC2C2C2),
  Color(0xFF8A8A8A),
];

/// Quantize by contrast against the canvas, not by raw luminance: that keeps the
/// mapping correct for both the dark- and light-tuned palettes (in each, a color
/// that stood out from its canvas stays the strongest ink).
Color _monoInk(Color color, Color canvas) {
  final ramp = CoduxTheme.isLight ? _monoLightRamp : _monoDarkRamp;
  final contrast = _contrastRatio(color, canvas);
  if (contrast >= 7.0) return ramp[0];
  if (contrast >= 3.0) return ramp[1];
  return ramp[2];
}

/// Single band shade for an explicit (non-inverse) cell background in mono mode.
Color _monoBand() =>
    CoduxTheme.isLight ? const Color(0xFFDCDCDC) : const Color(0xFF333333);

Color _ensureReadable(Color fg, Color bg) {
  if (_contrastRatio(fg, bg) >= 3.0 || !_isNeutral(fg)) return fg;
  return bg.computeLuminance() > 0.5 ? AppColors.terminalBg : AppColors.terminalText;
}

double _contrastRatio(Color a, Color b) {
  final l1 = a.computeLuminance();
  final l2 = b.computeLuminance();
  final lighter = l1 > l2 ? l1 : l2;
  final darker = l1 > l2 ? l2 : l1;
  return (lighter + 0.05) / (darker + 0.05);
}

/// A neutral host background on the same side as our canvas — a near-white cell
/// on a dark terminal, or a near-black cell on a light one. Either way it blows
/// out against the surrounding surface, so it is remapped to the elevated token.
bool _isClashingSurface(Color color) {
  if (!_isNeutral(color)) return false;
  final luminance = color.computeLuminance();
  return CoduxTheme.isLight ? luminance < 0.22 : luminance > 0.78;
}

bool _isNeutral(Color color) {
  final r = (color.r * 255).round();
  final g = (color.g * 255).round();
  final b = (color.b * 255).round();
  final maxChannel = [r, g, b].reduce((a, b) => a > b ? a : b);
  final minChannel = [r, g, b].reduce((a, b) => a < b ? a : b);
  return maxChannel - minChannel <= 24;
}
