import 'package:codux_protocol_ffi/codux_protocol_ffi.dart';
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
      fg: const TerminalScreenColor.named('Black'),
      bg: TerminalScreenColor.defaults,
      inverse: false,
    );

    expect(colors.bg, AppColors.terminalBg);
    expect(colors.fg, AppColors.textPrimary);
    expect(colors.drawBackground, isFalse);
  });

  test('dark terminal theme keeps readable ansi colors intact', () {
    final colors = TerminalTheme.resolveCellColors(
      fg: const TerminalScreenColor.named('Green'),
      bg: TerminalScreenColor.defaults,
      inverse: false,
    );

    expect(colors.fg, isNot(AppColors.textPrimary));
  });

  test('dark terminal theme preserves explicit rgb gradients', () {
    final first = TerminalTheme.resolveCellColors(
      fg: TerminalScreenColor.rgb(68, 72, 78),
      bg: TerminalScreenColor.defaults,
      inverse: false,
    );
    final second = TerminalTheme.resolveCellColors(
      fg: TerminalScreenColor.rgb(88, 92, 98),
      bg: TerminalScreenColor.defaults,
      inverse: false,
    );

    expect(first.fg, const Color(0xFF44484E));
    expect(second.fg, const Color(0xFF585C62));
    expect(first.fg, isNot(second.fg));
  });

  test('dark terminal theme dims faint foregrounds toward background', () {
    final normal = TerminalTheme.resolveCellColors(
      fg: TerminalScreenColor.rgb(230, 237, 243),
      bg: TerminalScreenColor.defaults,
      inverse: false,
      dim: false,
    );
    final dimmed = TerminalTheme.resolveCellColors(
      fg: TerminalScreenColor.rgb(230, 237, 243),
      bg: TerminalScreenColor.defaults,
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
      fg: const TerminalScreenColor.named('Black'),
      bg: const TerminalScreenColor.named('White'),
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
        fg: TerminalScreenColor.indexed(index),
        bg: TerminalScreenColor.defaults,
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
      fg: const TerminalScreenColor.named('BrightBlack'),
      bg: TerminalScreenColor.defaults,
      inverse: false,
    );
    final indexed = TerminalTheme.resolveCellColors(
      fg: const TerminalScreenColor.indexed(8),
      bg: TerminalScreenColor.defaults,
      inverse: false,
    );

    expect(named.fg, indexed.fg);
  });

  test('light terminal theme normalizes host dark cell backgrounds', () {
    CoduxTheme.brightness = Brightness.light;

    final colors = TerminalTheme.resolveCellColors(
      fg: const TerminalScreenColor.named('White'),
      bg: TerminalScreenColor.rgb(18, 18, 18),
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
      fg: TerminalScreenColor.rgb(105, 219, 124),
      bg: TerminalScreenColor.defaults,
      inverse: false,
    );

    expect(
      _contrast(colors.fg, AppColors.terminalBg),
      greaterThanOrEqualTo(3.0),
    );
  });

  test('light terminal theme darkens unreadable rgb underline colors', () {
    CoduxTheme.brightness = Brightness.light;

    final color = TerminalTheme.resolveUnderlineColor(
      TerminalScreenColor.rgb(105, 219, 124),
      AppColors.terminalText,
      AppColors.terminalBg,
    );

    expect(_contrast(color, AppColors.terminalBg), greaterThanOrEqualTo(3.0));
  });

  test('mono terminal palette collapses hues onto a readable gray ramp', () {
    CoduxTheme.brightness = Brightness.light;
    CoduxTheme.terminalMono = true;

    final shades = <Color>{};
    for (final index in [1, 2, 3, 4, 5, 6, 8, 15]) {
      final colors = TerminalTheme.resolveCellColors(
        fg: TerminalScreenColor.indexed(index),
        bg: TerminalScreenColor.defaults,
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

  test('mono terminal palette also collapses underline colors', () {
    CoduxTheme.brightness = Brightness.light;
    CoduxTheme.terminalMono = true;

    final color = TerminalTheme.resolveUnderlineColor(
      const TerminalScreenColor.indexed(1),
      AppColors.terminalText,
      AppColors.terminalBg,
    );

    expect(color.r, color.g);
    expect(color.g, color.b);
    expect(_contrast(color, AppColors.terminalBg), greaterThanOrEqualTo(4.5));
  });

  test('resolved cells are not cached across a palette switch', () {
    final dark = TerminalTheme.resolveCellColors(
      fg: const TerminalScreenColor.indexed(2),
      bg: TerminalScreenColor.defaults,
      inverse: false,
    );

    CoduxTheme.brightness = Brightness.light;
    final light = TerminalTheme.resolveCellColors(
      fg: const TerminalScreenColor.indexed(2),
      bg: TerminalScreenColor.defaults,
      inverse: false,
    );

    expect(light.fg, isNot(dark.fg));
  });
}
