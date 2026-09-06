## ADDED Requirements

### Requirement: Relay Registration Is Not A Connect Precondition
A controller or host endpoint SHALL treat relay registration as a best-effort signal. Failure or timeout of relay registration SHALL NOT close the endpoint, SHALL NOT abort connection establishment, and SHALL NOT prevent a dial to direct addresses already known for the peer. Relay remains the preferred path whenever it is available.

#### Scenario: Relay unreachable but peer reachable directly
- **GIVEN** a controller whose relay is unreachable
- **AND** a stored transport candidate carrying a direct address the host is currently reachable on
- **WHEN** the controller connects
- **THEN** the controller SHALL dial that direct address
- **AND** the connection SHALL be established without a relay

#### Scenario: Endpoint survives relay timeout
- **GIVEN** a controller endpoint whose relay registration times out
- **WHEN** the timeout elapses
- **THEN** the endpoint SHALL remain open and usable for dialling
- **AND** a later relay recovery SHALL bring the same endpoint online without rebuilding it

#### Scenario: Relay available is unchanged
- **GIVEN** a controller whose relay is reachable
- **WHEN** the controller connects
- **THEN** relay registration SHALL complete as before
- **AND** the dial schedule SHALL be the relay-routed schedule

#### Scenario: Host starts without a relay
- **GIVEN** a host whose relay is unreachable at startup
- **WHEN** the host starts its remote transport
- **THEN** startup SHALL succeed
- **AND** the host SHALL publish a transport candidate carrying a usable ticket
- **AND** the candidate relay URL MAY be empty

### Requirement: Relay-less Dial Fails No Slower Than The Former Gate
The dial schedule SHALL be derived from whether the endpoint has a relay home. A relay-less dial SHALL use a bounded flat schedule whose total budget does not exceed the relay registration timeout it replaces. A relay-routed dial SHALL retain its escalating schedule. Exhausting a relay-less dial SHALL report an error distinct from relay unreachability.

#### Scenario: Relay-less failure is not slower than before
- **GIVEN** a controller with no relay home and no reachable direct address
- **WHEN** the controller connects and every dial fails
- **THEN** total time to report failure SHALL NOT exceed the relay registration timeout budget

#### Scenario: Failure cause is distinguishable
- **GIVEN** a controller with no relay home
- **WHEN** every direct address dial fails
- **THEN** the reported error SHALL identify that direct addresses were attempted and unreachable
- **AND** it SHALL differ from the error reported when relay registration alone failed

### Requirement: One Reconnect Authority
Exactly one component SHALL advance the reconnect attempt counter and schedule reconnection. A single connection failure SHALL advance the counter once regardless of how many layers observe that failure. Automatic reconnection SHALL terminate after a bounded number of consecutive failures and SHALL report a resumable state. A successful connection SHALL reset the counter and the termination state.

#### Scenario: One failure advances the counter once
- **GIVEN** a controller whose connection attempt fails
- **AND** the failure is observed both as a transport state event and as a rejected connect result
- **WHEN** the failure is handled
- **THEN** the reconnect attempt counter SHALL advance by exactly one
- **AND** exactly one reconnect SHALL be scheduled

#### Scenario: Sustained failure stops retrying
- **GIVEN** a controller that has failed to connect for the bounded number of consecutive attempts
- **WHEN** the next failure is handled
- **THEN** no further automatic reconnect SHALL be scheduled
- **AND** the connection state SHALL be reported as resumable rather than reconnecting

#### Scenario: Success resets the policy
- **GIVEN** a controller that has accumulated consecutive failures
- **WHEN** a connection succeeds
- **THEN** the attempt counter SHALL reset
- **AND** a later failure SHALL restart the schedule from its shortest delay

#### Scenario: Stale failure does not schedule
- **GIVEN** a connection attempt that is superseded by a newer connection generation
- **WHEN** the superseded attempt reports failure
- **THEN** no reconnect SHALL be scheduled for the superseded target

### Requirement: Controller Retains Last Known Good Address
A controller SHALL record the transport address that carried its most recent successful connection and SHALL include that address in subsequent dials alongside the addresses carried by its stored pairing ticket. The record SHALL be optional in storage; a device record written without it SHALL remain readable and SHALL behave as if no address were remembered.

#### Scenario: Remembered address is dialled
- **GIVEN** a controller that previously connected over one direct address
- **WHEN** the controller dials again
- **THEN** the dial address set SHALL include the remembered address
- **AND** it SHALL also include the ticket's addresses

#### Scenario: Legacy record still loads
- **GIVEN** a stored device record written before this field existed
- **WHEN** the controller loads it and connects
- **THEN** the connection SHALL proceed using ticket addresses only
- **AND** the record SHALL gain the remembered address after the first success

#### Scenario: Stale remembered address does not displace the ticket
- **GIVEN** a remembered address that has failed consistently
- **WHEN** the controller dials
- **THEN** the ticket's addresses SHALL still be attempted

### Requirement: Host Announces Current Address On The Control Link
A host SHALL advertise an address-announcement capability and SHALL send its current endpoint address set to controllers that advertised support, on connection establishment and whenever its address set changes. A receiving controller SHALL replace and persist its stored transport candidate from the announcement. The message is additive: a host SHALL NOT send it to a controller that did not advertise support, and a controller that never receives it SHALL retain current behaviour.

#### Scenario: Address change reaches the controller
- **GIVEN** a paired controller and host that both support address announcement
- **AND** the host's address set changes after pairing
- **WHEN** the control link is established
- **THEN** the host SHALL announce its current address set
- **AND** the controller SHALL replace and persist its stored transport candidate

#### Scenario: New host with old controller
- **GIVEN** a host supporting address announcement
- **AND** a controller that did not advertise support
- **WHEN** the control link is established
- **THEN** the host SHALL NOT send the announcement
- **AND** the controller SHALL operate exactly as before

#### Scenario: Old host with new controller
- **GIVEN** a host that does not support address announcement
- **AND** a controller that does
- **WHEN** the control link is established
- **THEN** the controller SHALL retain its ticket addresses
- **AND** its last-known-good address SHALL remain its only address-tracking mechanism

#### Scenario: Announcement and local memory stay independent
- **GIVEN** a controller that has both received an announcement and recorded a last-known-good address
- **WHEN** the controller dials
- **THEN** replacing the stored candidate from an announcement SHALL NOT erase the remembered address
