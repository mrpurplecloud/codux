import 'package:codux_flutter/theme/app_theme.dart';
import 'package:codux_flutter/theme/terminal_theme.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

double _contrast(Color a, Color b) {
  final first = a.computeLuminance();
  final second = b.computeLuminance();
  final lighter = first > second ? first : second;
  final darker = first > second ? second : first;
  return (lighter + 0.05) / (darker + 0.05);
}

void main() {
  setUp(() {
    CoduxTheme.brightness = Brightness.dark;
    CoduxTheme.terminalMono = false;
  });

  tearDown(() {
    CoduxTheme.brightness = Brightness.dark;
    CoduxTheme.terminalMono = false;
  });

  test('dark terminal theme remaps unreadable neutral foregrounds', () {
    final colors = TerminalTheme.resolveCellColors(
      fg: {'kind': 'named', 'name': 'Black'},
      bg: {'kind': 'default'},
      inverse: false,
    );

    expect(colors.bg, AppColors.terminalBg);
    expect(colors.fg, AppColors.textPrimary);
    expect(colors.drawBackground, isFalse);
  });

  test('dark terminal theme keeps readable ansi colors intact', () {
    final colors = TerminalTheme.resolveCellColors(
      fg: {'kind': 'named', 'name': 'Green'},
      bg: {'kind': 'default'},
      inverse: false,
    );

    expect(colors.fg, isNot(AppColors.textPrimary));
  });

  test('dark terminal theme preserves explicit rgb gradients', () {
    final first = TerminalTheme.resolveCellColors(
      fg: {'kind': 'rgb', 'r': 68, 'g': 72, 'b': 78},
      bg: {'kind': 'default'},
      inverse: false,
    );
    final second = TerminalTheme.resolveCellColors(
      fg: {'kind': 'rgb', 'r': 88, 'g': 92, 'b': 98},
      bg: {'kind': 'default'},
      inverse: false,
    );

    expect(first.fg, const Color(0xFF44484E));
    expect(second.fg, const Color(0xFF585C62));
    expect(first.fg, isNot(second.fg));
  });

  test('dark terminal theme dims faint foregrounds toward background', () {
    final normal = TerminalTheme.resolveCellColors(
      fg: {'kind': 'rgb', 'r': 230, 'g': 237, 'b': 243},
      bg: {'kind': 'default'},
      inverse: false,
      dim: false,
    );
    final dimmed = TerminalTheme.resolveCellColors(
      fg: {'kind': 'rgb', 'r': 230, 'g': 237, 'b': 243},
      bg: {'kind': 'default'},
      inverse: false,
      dim: true,
    );

    expect(dimmed.fg, isNot(normal.fg));
    expect(
      dimmed.fg.computeLuminance(),
      lessThan(normal.fg.computeLuminance()),
    );
    expect(
      dimmed.fg.computeLuminance(),
      greaterThan(AppColors.bgBase.computeLuminance()),
    );
  });

  test('dark terminal theme normalizes host light cell backgrounds', () {
    final colors = TerminalTheme.resolveCellColors(
      fg: {'kind': 'named', 'name': 'Black'},
      bg: {'kind': 'named', 'name': 'White'},
      inverse: false,
    );

    expect(colors.bg, AppColors.terminalElevated);
    expect(colors.fg, AppColors.textPrimary);
    expect(colors.drawBackground, isTrue);
  });

  test('light terminal theme keeps ansi colors readable on the canvas', () {
    CoduxTheme.brightness = Brightness.light;

    for (final index in [1, 2, 3, 4, 5, 6, 9, 10, 11, 12, 13, 14]) {
      final colors = TerminalTheme.resolveCellColors(
        fg: {'kind': 'indexed', 'index': index},
        bg: {'kind': 'default'},
        inverse: false,
      );

      expect(
        _contrast(colors.fg, AppColors.terminalBg),
        greaterThanOrEqualTo(4.5),
        reason: 'ansi $index unreadable on the light canvas',
      );
    }
  });

  test('named and indexed slots resolve through the same palette', () {
    CoduxTheme.brightness = Brightness.light;

    final named = TerminalTheme.resolveCellColors(
      fg: {'kind': 'named', 'name': 'BrightBlack'},
      bg: {'kind': 'default'},
      inverse: false,
    );
    final indexed = TerminalTheme.resolveCellColors(
      fg: {'kind': 'indexed', 'index': 8},
      bg: {'kind': 'default'},
      inverse: false,
    );

    expect(named.fg, indexed.fg);
  });

  test('light terminal theme normalizes host dark cell backgrounds', () {
    CoduxTheme.brightness = Brightness.light;

    final colors = TerminalTheme.resolveCellColors(
      fg: {'kind': 'named', 'name': 'White'},
      bg: {'kind': 'rgb', 'r': 18, 'g': 18, 'b': 18},
      inverse: false,
    );

    expect(colors.bg, AppColors.terminalElevated);
    expect(colors.drawBackground, isTrue);
    expect(_contrast(colors.fg, colors.bg), greaterThanOrEqualTo(3.0));
  });

  test('light terminal theme darkens unreadable host rgb foregrounds', () {
    CoduxTheme.brightness = Brightness.light;

    final colors = TerminalTheme.resolveCellColors(
      // A pastel straight from a dark-tuned host palette.
      fg: {'kind': 'rgb', 'r': 105, 'g': 219, 'b': 124},
      bg: {'kind': 'default'},
      inverse: false,
    );

    expect(_contrast(colors.fg, AppColors.terminalBg), greaterThanOrEqualTo(3.0));
  });

  test('mono terminal palette collapses hues onto a readable gray ramp', () {
    CoduxTheme.brightness = Brightness.light;
    CoduxTheme.terminalMono = true;

    final shades = <Color>{};
    for (final index in [1, 2, 3, 4, 5, 6, 8, 15]) {
      final colors = TerminalTheme.resolveCellColors(
        fg: {'kind': 'indexed', 'index': index},
        bg: {'kind': 'default'},
        inverse: false,
      );
      shades.add(colors.fg);
      expect(
        _contrast(colors.fg, AppColors.terminalBg),
        greaterThanOrEqualTo(4.5),
        reason: 'mono ansi $index unreadable on the e-ink canvas',
      );
    }

    expect(shades.length, lessThanOrEqualTo(3));
    expect(AppColors.terminalBg, const Color(0xFFFFFFFF));
  });

  test('resolved cells are not cached across a palette switch', () {
    final dark = TerminalTheme.resolveCellColors(
      fg: {'kind': 'indexed', 'index': 2},
      bg: {'kind': 'default'},
      inverse: false,
    );

    CoduxTheme.brightness = Brightness.light;
    final light = TerminalTheme.resolveCellColors(
      fg: {'kind': 'indexed', 'index': 2},
      bg: {'kind': 'default'},
      inverse: false,
    );

    expect(light.fg, isNot(dark.fg));
  });
}
