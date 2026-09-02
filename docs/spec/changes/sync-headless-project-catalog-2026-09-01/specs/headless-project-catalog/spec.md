## ADDED Requirements

### Requirement: Headless Project Catalog Authority
Each Headless host SHALL own one persistent project catalog that is authoritative for project membership, host project ID, name, and path for every controller connected to that host. Catalogs from different Headless hosts SHALL remain isolated. Selected project and other client UI state SHALL remain per-client.

#### Scenario: Two controllers receive same catalog
- **GIVEN** Desktop and Mobile are connected to the same Headless host
- **WHEN** either controller obtains current project catalog
- **THEN** both SHALL observe same project IDs, names, paths, and membership

#### Scenario: Different hosts remain isolated
- **GIVEN** one controller connects to Headless hosts A and B
- **WHEN** both hosts contain equal paths or equal host project IDs
- **THEN** controller SHALL scope each entry by host device identity
- **AND** changes on host A SHALL NOT modify host B catalog

#### Scenario: Selection stays local
- **GIVEN** two clients display same Headless catalog
- **WHEN** client A selects another project
- **THEN** client B selected project SHALL remain unchanged

### Requirement: Headless Project Mutation Semantics
Headless SHALL support `project.add`, `project.edit`, and `project.remove`. A successful mutation SHALL commit before success is acknowledged. Add SHALL be idempotent by host-native normalized path, edit SHALL preserve host project ID, and remove SHALL affect catalog membership only.

#### Scenario: Add is idempotent by path
- **GIVEN** catalog already contains a project for one normalized host path
- **WHEN** a client retries `project.add` for equivalent path syntax
- **THEN** catalog SHALL contain one entry for path
- **AND** existing host project ID and name SHALL remain unchanged

#### Scenario: Edit preserves identity
- **GIVEN** catalog contains a project
- **WHEN** a client edits its name or path to a path not owned by another entry
- **THEN** Headless SHALL update requested fields
- **AND** host project ID SHALL remain unchanged

#### Scenario: Edit rejects path collision
- **GIVEN** projects A and B own different normalized paths
- **WHEN** a client edits project A to project B path
- **THEN** Headless SHALL return an error
- **AND** catalog SHALL remain unchanged

#### Scenario: Remove does not delete source files
- **GIVEN** catalog contains a project whose directory exists
- **WHEN** a client removes project from catalog
- **THEN** project SHALL be absent from later catalog snapshots
- **AND** directory and its contents SHALL remain unchanged

#### Scenario: Failed mutation publishes nothing
- **WHEN** validation or durable save fails
- **THEN** Headless SHALL return an error
- **AND** SHALL NOT acknowledge success or publish a newer project snapshot

### Requirement: Versioned Project Resource Subscription
Headless SHALL advertise `projects` in resource-subscription capabilities. A project-resource subscription SHALL receive a baseline when requested. Every committed catalog mutation SHALL publish one versioned full `project.list` snapshot to all current project subscribers and to mutation source.

#### Scenario: Subscription returns baseline
- **WHEN** a client sends `resource.subscribe` for `projects` with `baseline: true`
- **THEN** Headless SHALL register client
- **AND** SHALL send current full `project.list` with project resource version

#### Scenario: Mutation fans out to every controller
- **GIVEN** clients A and B subscribe to project resource
- **WHEN** client A commits project mutation
- **THEN** clients A and B SHALL receive full post-mutation catalog
- **AND** snapshots SHALL carry same newer resource version

#### Scenario: Unsubscribed or disconnected client stops receiving updates
- **GIVEN** a client previously subscribed to project resource
- **WHEN** client unsubscribes or disconnects
- **THEN** later project mutations SHALL NOT be sent to that client

#### Scenario: Stale snapshot cannot roll client backward
- **GIVEN** a client applied project snapshot version N for current host runtime instance
- **WHEN** it receives duplicate or older version
- **THEN** client SHALL ignore that snapshot

#### Scenario: New runtime begins new version epoch
- **GIVEN** Headless restarts with same persisted catalog
- **WHEN** client reconnects to new runtime instance
- **THEN** client SHALL reset project-version guard
- **AND** SHALL apply new baseline regardless of previous runtime version number

### Requirement: Durable and Concurrent Catalog Storage
Headless SHALL serialize catalog mutations and persist complete JSON through atomic replacement. Missing catalog SHALL mean empty state. Existing unreadable or malformed catalog SHALL produce an error and SHALL NOT be treated as empty during mutation.

#### Scenario: Concurrent distinct adds preserve both projects
- **WHEN** two clients concurrently add two distinct paths
- **THEN** committed catalog SHALL contain both entries
- **AND** neither mutation SHALL overwrite other mutation

#### Scenario: Concurrent equivalent adds deduplicate
- **WHEN** two clients concurrently add equivalent normalized path
- **THEN** committed catalog SHALL contain one entry for path
- **AND** both clients SHALL converge on same host project ID

#### Scenario: Restart preserves catalog
- **GIVEN** Headless committed project catalog
- **WHEN** process restarts using same data directory
- **THEN** first `project.list` SHALL reproduce committed catalog

#### Scenario: Malformed file is not overwritten
- **GIVEN** existing catalog file contains malformed JSON
- **WHEN** client requests mutation
- **THEN** Headless SHALL report persistence error
- **AND** malformed file bytes SHALL remain available for recovery

#### Scenario: Atomic save failure keeps previous catalog
- **GIVEN** valid catalog exists
- **WHEN** replacement of newly serialized catalog fails
- **THEN** previous complete catalog SHALL remain readable
- **AND** no newer snapshot SHALL be broadcast

### Requirement: Desktop Remote Project Projection
Desktop SHALL represent each Headless catalog item as a controller-local projection scoped by device ID and `hostProjectId`. Reconciliation SHALL update shared fields from Headless while preserving controller-local ID and local-only metadata. It SHALL never modify projects belonging to local, WSL, or another host target.

#### Scenario: Baseline creates projection
- **GIVEN** Headless baseline contains project unknown to Desktop
- **WHEN** Desktop reconciles baseline
- **THEN** Desktop SHALL create remote projection with source device ID and host project ID

#### Scenario: Reconciliation preserves local metadata
- **GIVEN** Desktop projection has badges, environment settings, ordering, or layout references
- **WHEN** newer Headless snapshot changes shared name or path
- **THEN** Desktop SHALL update shared fields
- **AND** SHALL preserve controller-only fields and Desktop project ID

#### Scenario: Authoritative removal prunes managed projection
- **GIVEN** migrated Desktop projection is absent from newer Headless snapshot
- **WHEN** Desktop reconciles snapshot
- **THEN** Desktop SHALL remove that projection
- **AND** SHALL leave unrelated target projects unchanged

#### Scenario: Disconnected cache is read-only
- **GIVEN** Desktop previously synchronized Headless catalog and is now disconnected
- **WHEN** user views project list
- **THEN** Desktop MAY display last synchronized projection
- **BUT** remote mutation SHALL NOT create authoritative local-only membership

### Requirement: Desktop Host Project ID Translation
Desktop SHALL retain separate controller project ID and host project ID. Every outbound Headless project-scoped operation SHALL use `hostProjectId` as protocol `projectId`; inbound Headless relationships SHALL map host ID back to controller projection ID. Path SHALL NOT replace explicit identity when host ID is known.

#### Scenario: Terminal request uses host identity
- **GIVEN** Desktop projection ID differs from its `hostProjectId`
- **WHEN** Desktop creates terminal for project
- **THEN** outbound request `projectId` SHALL equal `hostProjectId`

#### Scenario: Other project-scoped requests use host identity
- **GIVEN** Desktop projection ID differs from its `hostProjectId`
- **WHEN** Desktop invokes Git, AI, file, or worktree operation scoped to project
- **THEN** outbound protocol `projectId` SHALL equal `hostProjectId`

#### Scenario: Same host ID on different devices does not collide
- **GIVEN** two Headless devices advertise same host project ID
- **WHEN** Desktop stores both projections
- **THEN** projections SHALL have distinct controller identity through device scope

### Requirement: Legacy Desktop Remote Project Migration
Desktop SHALL perform one idempotent migration per Headless device after first valid baseline from host advertising project subscriptions. Migration SHALL register legacy remote paths on Headless, preserve controller IDs and local metadata, and mark completion only after final reconciliation succeeds.

#### Scenario: Legacy record becomes shared
- **GIVEN** Desktop has remote record without `hostProjectId`
- **AND** upgraded Headless catalog lacks that path
- **WHEN** first migration completes
- **THEN** Headless catalog SHALL contain path
- **AND** Desktop record SHALL retain controller ID and gain matching host project ID

#### Scenario: Existing Headless entry wins
- **GIVEN** legacy Desktop record and Headless entry have equivalent normalized path
- **WHEN** migration runs
- **THEN** no duplicate SHALL be created
- **AND** existing Headless name and host project ID SHALL remain authoritative

#### Scenario: Partial failure is retryable
- **WHEN** any legacy registration or final reconciliation fails
- **THEN** Desktop SHALL NOT store migration-complete marker
- **AND** SHALL NOT prune remaining legacy records
- **AND** later retry SHALL remain idempotent

#### Scenario: Completed migration does not resurrect deletion
- **GIVEN** Desktop stored migration-complete marker
- **AND** project is later removed from Headless
- **WHEN** Desktop reconnects
- **THEN** Desktop SHALL remove cached projection
- **AND** SHALL NOT upload it again

### Requirement: Capability-Gated Compatibility
Project catalog sharing SHALL be additive and capability-gated. Existing one-shot project operations SHALL remain usable. Clients connected to host without `projects` subscription capability SHALL retain pre-change behavior rather than assuming live authoritative synchronization.

#### Scenario: New client connects to old Headless
- **GIVEN** Headless does not advertise project resource subscription
- **WHEN** new Desktop or Mobile connects
- **THEN** client SHALL NOT require subscription flow
- **AND** supported one-shot project operations SHALL continue

#### Scenario: Old client connects to new Headless
- **GIVEN** upgraded Headless supports project sharing
- **WHEN** old client sends `project.list`, `project.add`, or `project.remove`
- **THEN** Headless SHALL continue accepting operation
- **AND** additive acknowledgement or broadcast messages MAY be ignored safely

### Requirement: Cross-Client Regression Protection
Implementation SHALL include deterministic automated coverage for original visibility failure and its persistence, identity, migration, and concurrency boundaries. Mobile production code SHALL change only when an automated test demonstrates incompatible behavior.

#### Scenario: Desktop-created project reaches Mobile
- **GIVEN** Desktop-style and Mobile-style clients subscribe to same Headless host
- **WHEN** Desktop-style client adds project
- **THEN** Mobile-style client SHALL receive project without reconnect or explicit list request

#### Scenario: Mobile-created project reaches Desktop
- **GIVEN** Desktop-style and Mobile-style clients subscribe to same Headless host
- **WHEN** Mobile-style client adds project
- **THEN** Desktop projection SHALL contain project without reconnect

#### Scenario: Existing project modes do not regress
- **WHEN** shared Headless catalog feature is enabled
- **THEN** local, WSL, and Desktop-host project tests SHALL retain existing behavior

