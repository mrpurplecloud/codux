/// The single authority over when a lost connection is retried, and when it
/// stops being retried.
///
/// **Why one authority.** A failed connection surfaces twice: the transport
/// reports a `failed:` state AND the connect future rejects. Both land on the
/// same teardown path, so a policy that simply counted calls advanced twice per
/// failure — reaching its ceiling in half the intended number of failures, with
/// an attempt number in the logs that was double the truth. Failures are
/// therefore identified by the connection generation they belong to: the first
/// observation of a generation owns it, and any later observation of that same
/// generation is the same failure being reported again, not a new one.
///
/// **Why it terminates.** Retrying forever at the ceiling interval is not
/// resilience, it is a background drain on a phone whose owner has walked away
/// from a host that is unreachable for structural reasons. After
/// [maxAttempts] consecutive failures the policy is exhausted and schedules
/// nothing; the caller surfaces a resumable state instead. Anything that
/// genuinely changes the odds — a successful connection, a network route
/// change, the user asking again — calls [reset].
class ReconnectPolicy {
  ReconnectPolicy({
    this.baseDelay = const Duration(milliseconds: 800),
    this.ceilingDelay = const Duration(milliseconds: 25600),
  });

  /// Delay before the first retry. Each subsequent attempt doubles it until
  /// [ceilingDelay].
  final Duration baseDelay;

  /// Upper bound on the retry delay.
  final Duration ceilingDelay;

  /// Consecutive failures tolerated before the policy stops scheduling.
  ///
  /// Twelve spans roughly three and a half minutes: six doublings from
  /// [baseDelay] to [ceilingDelay] (~50s), then six more at the ceiling
  /// (~150s). Long enough to ride out a lift, a tunnel, or a Wi-Fi handover;
  /// short enough that a host which is simply gone stops costing battery.
  static const int maxAttempts = 12;

  int _attempt = 0;
  int? _failedGeneration;
  bool _exhausted = false;

  /// Attempts recorded since the last [reset].
  int get attempt => _attempt;

  /// Whether the policy has given up. The caller should present a resumable
  /// state rather than a perpetual "reconnecting".
  bool get exhausted => _exhausted;

  /// Records that the connection identified by [generation] failed, and returns
  /// how long to wait before retrying.
  ///
  /// Returns null when nothing should be scheduled. Check [exhausted] to tell
  /// the two reasons apart: exhausted means this connection has given up and
  /// the UI should say so; not exhausted means the failure was a duplicate or a
  /// stale generation and there is nothing to do.
  Duration? recordFailure({required int generation}) {
    if (_exhausted) return null;
    // Same generation: the other half of a failure already counted. Strictly
    // older: a superseded connect reporting in late, whose failure says nothing
    // about the connection that replaced it.
    final failed = _failedGeneration;
    if (failed != null && generation <= failed) return null;
    _failedGeneration = generation;
    _attempt += 1;
    if (_attempt > maxAttempts) {
      _attempt = maxAttempts;
      _exhausted = true;
      return null;
    }
    return _delayForAttempt(_attempt);
  }

  /// Clears the schedule and any exhaustion. Called on a successful connection
  /// and on every event that changes the odds of the next attempt.
  void reset() {
    _attempt = 0;
    _failedGeneration = null;
    _exhausted = false;
  }

  Duration _delayForAttempt(int attempt) {
    final doublings = attempt - 1;
    if (doublings >= 31) return ceilingDelay;
    final scaled = baseDelay.inMilliseconds << doublings;
    return scaled >= ceilingDelay.inMilliseconds
        ? ceilingDelay
        : Duration(milliseconds: scaled);
  }
}
