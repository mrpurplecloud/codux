import 'dart:async';

import 'package:codux_flutter/i18n.dart';
import 'package:codux_flutter/screens/scanner_screen.dart';
import 'package:codux_flutter/services/camera_support.dart';
import 'package:codux_flutter/theme/app_theme.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:mobile_scanner/mobile_scanner.dart';

/// Stands in for the real probe so the test never touches platform channels.
class _FakeCameraSupport extends CameraSupport {
  _FakeCameraSupport(this.availability);

  final CameraAvailability availability;
  int startAttempts = 0;

  @override
  Future<CameraAvailability> resolve() async => availability;

  @override
  Future<void> markStartAttempt() async => startAttempts++;

  @override
  Future<void> markStartSettled() async {}

  @override
  Future<void> clearCrashBlock() async {}
}

class _DelayedCameraSupport extends CameraSupport {
  final attemptStarted = Completer<void>();
  final releaseAttempt = Completer<void>();

  @override
  Future<CameraAvailability> resolve() async => CameraAvailability.available;

  @override
  Future<void> markStartAttempt() {
    attemptStarted.complete();
    return releaseAttempt.future;
  }

  @override
  Future<void> markStartSettled() async {}
}

class _FakeMobileScannerPlatform extends MobileScannerPlatform {
  _FakeMobileScannerPlatform({this.startError, this.stopGate});

  final MobileScannerException? startError;
  final Completer<void>? stopGate;
  int starts = 0;
  int stops = 0;

  @override
  Stream<BarcodeCapture?> get barcodesStream => const Stream.empty();

  @override
  Stream<TorchState> get torchStateStream => const Stream.empty();

  @override
  Stream<double> get zoomScaleStateStream => const Stream.empty();

  @override
  Widget buildCameraView() => const SizedBox.shrink();

  @override
  Future<MobileScannerViewAttributes> start(StartOptions startOptions) async {
    starts += 1;
    if (startError case final error?) throw error;
    return const MobileScannerViewAttributes(
      cameraDirection: CameraFacing.back,
      currentTorchMode: TorchState.unavailable,
      size: Size(200, 200),
      numberOfCameras: 1,
    );
  }

  @override
  Future<void> stop() async {
    stops += 1;
    await stopGate?.future;
  }

  @override
  Future<void> dispose() async {}
}

Future<void> _pumpScanner(
  WidgetTester tester,
  CameraSupport support, {
  ValueChanged<String>? onDetected,
}) async {
  await tester.pumpWidget(
    MaterialApp(
      theme: buildAppTheme(),
      home: Scaffold(
        body: AppPreferences(
          accent: AccentChoices.cyan,
          locale: LocaleChoices.simplifiedChinese,
          themeMode: ThemeMode.dark,
          child: Stack(
            children: [
              ScannerScreen(
                bottomInset: 0,
                onDetected: onDetected ?? (_) {},
                onClose: () {},
                cameraSupport: support,
              ),
            ],
          ),
        ),
      ),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('camera-less device never mounts the camera preview', (
    tester,
  ) async {
    final support = _FakeCameraSupport(CameraAvailability.missingHardware);

    await _pumpScanner(tester, support);

    // The native start is what kills the process on such devices, so neither
    // the preview nor a start attempt may happen.
    expect(find.byType(MobileScanner), findsNothing);
    expect(support.startAttempts, 0);
    expect(find.text(tr('pair.noCameraTitle', 'zh-CN')), findsOneWidget);
    expect(find.text(tr('pair.retryCamera', 'zh-CN')), findsNothing);
  });

  testWidgets('camera-less device still pairs through manual connect', (
    tester,
  ) async {
    String? payload;
    await _pumpScanner(
      tester,
      _FakeCameraSupport(CameraAvailability.missingHardware),
      onDetected: (value) => payload = value,
    );

    await tester.tap(find.text(tr('pair.manualConnect', 'zh-CN')));
    await tester.pumpAndSettle();
    await tester.enterText(
      find.widgetWithText(TextField, tr('pair.tokenHint', 'zh-CN')),
      'iroh-ticket-token',
    );
    await tester.pump();
    await tester.tap(find.text(tr('pair.submit', 'zh-CN')));
    await tester.pump();

    expect(payload, 'codux://pair?payload=iroh-ticket-token');
  });

  testWidgets('a crash block offers a retry, missing hardware does not', (
    tester,
  ) async {
    await _pumpScanner(
      tester,
      _FakeCameraSupport(CameraAvailability.blockedAfterFailure),
    );

    expect(find.byType(MobileScanner), findsNothing);
    expect(find.text(tr('pair.retryCamera', 'zh-CN')), findsOneWidget);
  });

  testWidgets('backgrounding while the start marker is pending cancels start', (
    tester,
  ) async {
    final previousPlatform = MobileScannerPlatform.instance;
    final platform = _FakeMobileScannerPlatform();
    MobileScannerPlatform.instance = platform;
    addTearDown(() => MobileScannerPlatform.instance = previousPlatform);
    final support = _DelayedCameraSupport();

    await _pumpScanner(tester, support);
    await support.attemptStarted.future;
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.paused);
    await tester.pump();
    support.releaseAttempt.complete();
    await tester.pumpAndSettle();

    expect(platform.starts, 0);
  });

  testWidgets('resume waits for an in-flight native stop before restarting', (
    tester,
  ) async {
    final previousPlatform = MobileScannerPlatform.instance;
    final stopGate = Completer<void>();
    final platform = _FakeMobileScannerPlatform(stopGate: stopGate);
    MobileScannerPlatform.instance = platform;
    addTearDown(() => MobileScannerPlatform.instance = previousPlatform);

    await _pumpScanner(
      tester,
      _FakeCameraSupport(CameraAvailability.available),
    );
    expect(platform.starts, 1);

    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.paused);
    await tester.pump();
    expect(platform.stops, 1);

    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    await tester.pump();
    expect(platform.starts, 1);

    stopGate.complete();
    await tester.pumpAndSettle();
    expect(platform.starts, 2);
  });

  testWidgets('a caught camera start failure offers retry', (tester) async {
    final previousPlatform = MobileScannerPlatform.instance;
    final platform = _FakeMobileScannerPlatform(
      startError: const MobileScannerException(
        errorCode: MobileScannerErrorCode.permissionDenied,
      ),
    );
    MobileScannerPlatform.instance = platform;
    addTearDown(() => MobileScannerPlatform.instance = previousPlatform);

    await _pumpScanner(
      tester,
      _FakeCameraSupport(CameraAvailability.available),
    );

    expect(platform.starts, 1);
    expect(find.text('摄像头不可用'), findsOneWidget);
    expect(find.text(tr('pair.retryCamera', 'zh-CN')), findsOneWidget);
  });
}
