import 'package:codux_flutter/services/reconnect_policy.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('one failed connection advances the attempt exactly once', () {
    final policy = ReconnectPolicy();

    // A single failed connect surfaces twice: once as a transport `failed:`
    // state event, once as the rejected connect future. Both carry the same
    // connection generation, so they are the same failure — not two.
    final first = policy.recordFailure(generation: 1);
    final second = policy.recordFailure(generation: 1);

    expect(first, isNotNull);
    expect(
      second,
      isNull,
      reason: 'the second observation is the same failure',
    );
    expect(policy.attempt, 1);
    expect(policy.exhausted, isFalse);
  });

  test('backoff doubles from the base delay up to the ceiling', () {
    final policy = ReconnectPolicy();
    final delays = <int>[];
    for (var generation = 1; generation <= 8; generation += 1) {
      final delay = policy.recordFailure(generation: generation);
      if (delay == null) break;
      delays.add(delay.inMilliseconds);
    }

    expect(delays.take(6), [800, 1600, 3200, 6400, 12800, 25600]);
    expect(
      delays.skip(6),
      everyElement(25600),
      reason: 'the delay must not grow past the ceiling',
    );
  });

  test('sustained failure exhausts the policy instead of retrying forever', () {
    final policy = ReconnectPolicy();
    var scheduled = 0;
    for (var generation = 1; generation <= 100; generation += 1) {
      if (policy.recordFailure(generation: generation) != null) scheduled += 1;
    }

    expect(policy.exhausted, isTrue);
    expect(scheduled, ReconnectPolicy.maxAttempts);
  });

  test('an exhausted policy schedules nothing further', () {
    final policy = ReconnectPolicy();
    for (var generation = 1; generation <= 100; generation += 1) {
      policy.recordFailure(generation: generation);
    }

    expect(policy.recordFailure(generation: 1000), isNull);
    expect(policy.exhausted, isTrue);
  });

  test('a successful connection resets the schedule and the exhaustion', () {
    final policy = ReconnectPolicy();
    for (var generation = 1; generation <= 100; generation += 1) {
      policy.recordFailure(generation: generation);
    }
    expect(policy.exhausted, isTrue);

    policy.reset();

    expect(policy.exhausted, isFalse);
    expect(policy.attempt, 0);
    expect(
      policy.recordFailure(generation: 1),
      const Duration(milliseconds: 800),
      reason: 'a later failure restarts from the shortest delay',
    );
  });

  test(
    'reset clears the recorded generation so the same one can fail again',
    () {
      final policy = ReconnectPolicy();
      expect(policy.recordFailure(generation: 7), isNotNull);
      expect(policy.recordFailure(generation: 7), isNull);

      policy.reset();

      expect(
        policy.recordFailure(generation: 7),
        isNotNull,
        reason:
            'after a reconnect cycle a repeated generation is a new failure',
      );
    },
  );

  test('a stale generation cannot advance the schedule', () {
    final policy = ReconnectPolicy();
    policy.recordFailure(generation: 5);
    final beforeAttempt = policy.attempt;

    // A superseded connect (the user picked another device, or a newer connect
    // replaced this one) reporting its failure late must not push the backoff
    // for the connection that replaced it.
    expect(policy.recordFailure(generation: 4), isNull);
    expect(policy.attempt, beforeAttempt);
  });
}
