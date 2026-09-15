## ADDED Requirements

### Requirement: Transport Loss Does Not End Agent Session
An established host agent session SHALL remain owned by the host when an iroh connection closes. Reconnect SHALL target the same session ID when that session remains available. A connection generation SHALL reject stale completions without changing the current session state.

#### Scenario: Short network interruption
- **GIVEN** a host agent session is running
- **AND** the controller's iroh connection closes temporarily
- **WHEN** a new connection is established
- **THEN** the host session SHALL continue rather than being recreated
- **AND** the controller SHALL attempt to restore the same session ID

#### Scenario: Stale connection completion
- **GIVEN** a connection attempt has been superseded by a newer generation
- **WHEN** the old attempt reports success or failure
- **THEN** the result SHALL be discarded
- **AND** it SHALL NOT overwrite current screen, retry state or session state

### Requirement: Terminal State Resumes From Contiguous Cursor
The controller SHALL report its last contiguous terminal `outputSeq` and terminal generation when restoring a session. The host SHALL send retained delta output when the cursor is covered by its bounded replay window; otherwise the host SHALL request or send one authoritative baseline. The controller SHALL publish restored state only after the selected delta or baseline has been applied.

#### Scenario: Cursor is retained
- **GIVEN** the controller has applied output through sequence N
- **AND** the host replay window still contains output after N
- **WHEN** the session reconnects
- **THEN** the host SHALL send output after N in order
- **AND** the controller SHALL not request a full baseline solely because the transport changed

#### Scenario: Cursor is expired
- **GIVEN** the controller cursor is older than the host replay window
- **WHEN** the session reconnects
- **THEN** the controller SHALL receive one authoritative baseline
- **AND** the controller SHALL reconcile live output using the baseline watermark

#### Scenario: Gap arrives during restore
- **GIVEN** live output arrives while a baseline or delta restore is in progress
- **WHEN** the restore completes
- **THEN** held output SHALL be replayed only when not covered by the restore watermark
- **AND** output order and screen correctness SHALL be preserved

### Requirement: Input Retry Is Idempotent Within Session
Each controller input SHALL carry a stable `inputId` for its session. The host SHALL deduplicate repeated IDs within the active session and SHALL return an ACK without executing a duplicate input. The controller SHALL retain unacknowledged input only within bounded count and time limits.

#### Scenario: ACK is lost
- **GIVEN** host accepted input ID X
- **AND** the controller did not receive the ACK
- **WHEN** the controller retries input ID X
- **THEN** the host SHALL return an ACK for X
- **AND** the host SHALL execute X at most once within that session

#### Scenario: Pending input expires
- **GIVEN** input ID X remains unacknowledged past the retention limit
- **WHEN** the limit is reached
- **THEN** the controller SHALL mark X as unconfirmed
- **AND** it SHALL stop automatic retries for X

#### Scenario: Host session restarts
- **GIVEN** the host session and its in-memory deduplication record have restarted
- **WHEN** a prior input ID is presented
- **THEN** the system SHALL not claim exactly-once execution
- **AND** any side-effecting ambiguity SHALL be surfaced as unconfirmed

### Requirement: Recovery Continues After Bounded Backoff
The reconnect coordinator SHALL own attempt advancement and scheduling. A single connection failure SHALL advance the attempt counter once. After the normal bounded backoff is exhausted, an active session intent SHALL enter a low-frequency parked probe instead of silently becoming permanently inactive. Parked probing SHALL stop when the session is closed, the user cancels recovery, or platform lifecycle policy disallows it.

#### Scenario: One failure has two observers
- **GIVEN** one failed connection is reported through transport state and a rejected connect result
- **WHEN** the coordinator handles both observations
- **THEN** the attempt counter SHALL advance once
- **AND** one next recovery action SHALL be scheduled

#### Scenario: Parked probe recovers
- **GIVEN** normal reconnect attempts are exhausted
- **AND** the host or relay becomes reachable later
- **WHEN** a parked probe succeeds
- **THEN** the coordinator SHALL restore the session
- **AND** a successful usable connection SHALL reset retry state

### Requirement: Foreground Resume Is Cache First
When a controller returns to the foreground, it SHALL publish its cached terminal screen and session metadata before waiting for network synchronization. It SHALL skip a baseline request when generation and contiguous cursor remain valid and no gap is observed.

#### Scenario: No gap on foreground
- **GIVEN** cached terminal state has no output gap
- **AND** the session generation is unchanged
- **WHEN** the controller returns to the foreground
- **THEN** the cached screen SHALL remain visible
- **AND** recovery SHALL use a lightweight liveness check without a full baseline

#### Scenario: Gap on foreground
- **GIVEN** cached terminal state has a sequence gap or stale generation
- **WHEN** the controller returns to the foreground
- **THEN** the controller SHALL keep cached state visible while restoring
- **AND** it SHALL request delta or baseline according to the cursor contract

### Requirement: Control and Input Remain Usable Under Output Backlog
The transport scheduler SHALL keep control, input, resize and Ctrl-C ahead of bulk terminal output and baseline transfer. Each queue SHALL be bounded. If output backlog prevents safe incremental delivery, the system SHALL request or send an authoritative baseline rather than silently corrupting terminal state.

#### Scenario: Large output burst
- **GIVEN** host emits output faster than the weak network can deliver
- **WHEN** output backlog reaches its bound
- **THEN** input and control messages SHALL remain schedulable
- **AND** recovery SHALL preserve final screen correctness through sequence and baseline rules

### Requirement: Connection Availability Is Reported By Stage
The controller SHALL distinguish transport open, protocol ready, host responsive and session restored. Direct/relay path, candidate, generation, attempt and failure category SHALL be available for diagnosis. A transport-open event alone SHALL not be reported as an available Codux session.

#### Scenario: QUIC opens but host does not respond
- **GIVEN** a QUIC connection is open
- **AND** protocol or host readiness has not completed
- **WHEN** status is shown
- **THEN** the status SHALL remain recoverable and not claim an interactive session

#### Scenario: Relay unavailable but direct path works
- **GIVEN** relay registration is unavailable
- **AND** a known direct candidate is reachable
- **WHEN** the controller connects
- **THEN** the direct path SHALL be attempted
- **AND** relay unavailability alone SHALL not make the session unavailable
