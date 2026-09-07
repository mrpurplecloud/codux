import 'package:codux_flutter/models/remote_models.dart';
import 'package:codux_flutter/services/device_snapshot_store.dart';
import 'package:flutter_test/flutter_test.dart';

RemoteTransportCandidate candidate({
  String nodeId = 'node-1',
  String relayUrl = 'https://relay.example',
  String ticket = 'ticket-v1',
}) => RemoteTransportCandidate(
  kind: RemoteTransportKind.iroh,
  nodeId: nodeId,
  relayUrl: relayUrl,
  ticket: ticket,
);

StoredDevice device({
  List<RemoteTransportCandidate>? transports,
  List<String> directAddrs = const [],
}) => StoredDevice(
  server: 'https://relay.example',
  hostId: 'host-1',
  deviceId: 'device-1',
  token: 'token-1',
  name: 'Phone',
  transports: transports ?? [candidate()],
  directAddrs: directAddrs,
);

void main() {
  group('RemoteTransportCandidate equality', () {
    test('candidates with the same fields compare equal', () {
      expect(candidate(), candidate());
      expect(candidate().hashCode, candidate().hashCode);
    });

    test('a changed ticket is a different candidate', () {
      expect(candidate(ticket: 'ticket-v2'), isNot(candidate()));
    });
  });

  group('DeviceTransportsReplaced', () {
    test('adopts the addresses the host just reported', () {
      final devices = [device()];
      final fresh = [candidate(ticket: 'ticket-v2')];

      final result = DeviceTransportsReplaced(
        'device-1',
        fresh,
      ).applyTo(devices);

      expect(result.single.transports, fresh);
    });

    test('reports no update when the host repeats what we already stored', () {
      final devices = [device()];

      final result = DeviceTransportsReplaced('device-1', [
        candidate(),
      ]).applyTo(devices);

      expect(
        result,
        same(devices),
        reason:
            'host.info arrives on every connection; an unchanged address '
            'set must not cost a disk write each time',
      );
    });

    test('an empty report leaves the stored addresses alone', () {
      final devices = [device()];

      final result = const DeviceTransportsReplaced(
        'device-1',
        [],
      ).applyTo(devices);

      expect(
        result,
        same(devices),
        reason:
            'a host that reports nothing has not told us to forget what '
            'we have — that would strand the device',
      );
      expect(result.first.transports, isNotEmpty);
    });

    test('adopting new addresses keeps the remembered direct addresses', () {
      final devices = [
        device(directAddrs: ['10.8.0.3:41234']),
      ];

      final result = DeviceTransportsReplaced('device-1', [
        candidate(ticket: 'ticket-v2'),
      ]).applyTo(devices);

      expect(
        result.single.directAddrs,
        ['10.8.0.3:41234'],
        reason:
            'the two address sources are independent fallbacks; one must '
            'not erase the other',
      );
    });

    test('reports no update for an unknown device', () {
      final devices = [device()];

      final result = DeviceTransportsReplaced('someone-else', [
        candidate(ticket: 'ticket-v2'),
      ]).applyTo(devices);

      expect(result, same(devices));
    });

    test('a relay-less reported candidate is adopted', () {
      // A host with no reachable relay reports an empty relayUrl and relies on
      // its ticket. Adopting that is the whole point: refusing it would pin the
      // device to a pairing-time address the host may have left.
      final devices = [device()];
      final relayLess = [candidate(relayUrl: '', ticket: 'ticket-v2')];

      final result = DeviceTransportsReplaced(
        'device-1',
        relayLess,
      ).applyTo(devices);

      expect(result.single.transports, relayLess);
      expect(
        result.single.transportByKind(RemoteTransportKind.iroh)?.ticket,
        'ticket-v2',
      );
    });

    test('adoption carries the relay authentication through', () {
      // Replacement owns the stored set, so anything a host omits is
      // lost on this side. A dropped bearer token would leave the device unable
      // to use a custom relay it had been using fine.
      final devices = [device()];
      const authenticated = RemoteTransportCandidate(
        kind: RemoteTransportKind.iroh,
        nodeId: 'node-1',
        relayUrl: 'https://relay.example',
        ticket: 'ticket-v2',
        relayAuthentication: 'bearer-token',
      );

      final result = const DeviceTransportsReplaced('device-1', [
        authenticated,
      ]).applyTo(devices);

      expect(
        result.single
            .transportByKind(RemoteTransportKind.iroh)
            ?.relayAuthentication,
        'bearer-token',
      );
    });
  });
}
