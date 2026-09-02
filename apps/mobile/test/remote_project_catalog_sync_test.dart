import 'package:codux_flutter/models/remote_models.dart';
import 'package:codux_flutter/services/remote_capabilities.dart';
import 'package:codux_flutter/services/remote_project_controller.dart';
import 'package:codux_flutter/services/remote_protocol.dart';
import 'package:codux_flutter/services/remote_resource_subscription_coordinator.dart';
import 'package:codux_flutter/services/remote_runtime_payloads.dart';
import 'package:codux_flutter/services/remote_runtime_store.dart';
import 'package:codux_flutter/services/remote_state_versions.dart';
import 'package:flutter_test/flutter_test.dart';

/// The headless host's `host.info`: it advertises the shared project catalog as
/// a subscribable resource, which is what makes this client a live view of it
/// rather than a poller.
const _headlessHostInfo = {
  'hostId': 'host-1',
  'app': 'codux-agent',
  'runtimeInstanceId': 'host-1-agent-0001',
  'capabilities': {
    'resourceSubscriptions': ['projects', 'terminals'],
  },
};

void main() {
  test('subscribes to the catalog when the headless host advertises it', () {
    final sent = <RelayEnvelope>[];
    final coordinator =
        RemoteResourceSubscriptionCoordinator(
          send: (envelope) {
            sent.add(envelope);
            return true;
          },
        )..configure(
          RemoteResourceSubscriptionCapability.fromHostInfo(_headlessHostInfo),
        );

    coordinator.requestGlobal(
      resource: RemoteResourceType.projects,
      fallback: RelayEnvelope(type: RemoteMessageType.projectList),
    );

    expect(sent.single.type, RemoteMessageType.resourceSubscribe);
    expect(sent.single.payload, containsPair('resource', 'projects'));
  });

  test('edit and remove address the project by the host catalog id', () {
    const controller = RemoteProjectController();
    const project = ProjectInfo(
      id: 'p-host-1',
      name: 'Alpha',
      path: '/srv/alpha',
    );

    final plan = controller.savePlan(
      mode: ProjectFormMode.edit,
      path: '/srv/renamed',
      name: 'Renamed',
      selectedProject: project,
    );

    // The host keeps that id through the edit it acknowledges, so the next
    // mutation addresses the same entry. (The acknowledgement itself is pinned
    // host-side: `edit_and_remove_fan_out_and_keep_the_host_project_id`.)
    expect((plan.envelope!.payload as Map)['projectId'], 'p-host-1');
    expect((plan.envelope!.payload as Map)['name'], 'Renamed');
    expect(controller.removeEnvelope(project).payload, {
      'projectId': 'p-host-1',
    });
  });

  test('an externally-originated catalog snapshot replaces the list', () {
    final versions = RemoteStateVersions();
    final store = RemoteRuntimeStore();
    store.applyProjectList(
      projects: remoteProjectsFromPayload(const {
        'projects': [
          {'id': 'p-host-1', 'name': 'Alpha', 'path': '/srv/alpha'},
          {'id': 'p-host-2', 'name': 'Beta', 'path': '/srv/beta'},
        ],
        'version': 4,
      }),
      remoteSelectedProjectId: null,
      remoteSelectedWorktreeId: null,
      terminalVisible: false,
      terminalListLoaded: false,
    );
    versions.accept('projects', version: 4);
    store.projectSelected(projectId: 'p-host-2', worktreeId: null);

    // Another controller renamed one project and removed the other; the host
    // pushes the committed catalog without this client asking for it.
    const pushed = {
      'projects': [
        {'id': 'p-host-2', 'name': 'Beta renamed', 'path': '/srv/beta'},
      ],
      'version': 5,
    };
    expect(
      versions.accept(
        'projects',
        version: remoteStateVersionFromPayload(pushed),
      ),
      isTrue,
    );
    store.applyProjectList(
      projects: remoteProjectsFromPayload(pushed),
      remoteSelectedProjectId: null,
      remoteSelectedWorktreeId: null,
      terminalVisible: false,
      terminalListLoaded: false,
    );

    expect(store.projects.map((project) => project.id), ['p-host-2']);
    expect(store.projects.single.name, 'Beta renamed');
    // Selection is this client's own, and the host never sends one.
    expect(store.selectedProjectId, 'p-host-2');
  });

  test('a stale catalog version never reaches the project list', () {
    final versions = RemoteStateVersions();
    final store = RemoteRuntimeStore();
    const current = {
      'projects': [
        {'id': 'p-host-1', 'name': 'Alpha', 'path': '/srv/alpha'},
      ],
      'version': 5,
    };
    expect(
      versions.accept(
        'projects',
        version: remoteStateVersionFromPayload(current),
      ),
      isTrue,
    );
    store.applyProjectList(
      projects: remoteProjectsFromPayload(current),
      remoteSelectedProjectId: null,
      remoteSelectedWorktreeId: null,
      terminalVisible: false,
      terminalListLoaded: false,
    );

    // An older snapshot from the dropped connection arrives late; the guard
    // stops it before the store ever sees it.
    expect(versions.accept('projects', version: 4), isFalse);
    expect(store.projects.map((project) => project.id), ['p-host-1']);
    // A re-delivery of the applied version carries that same catalog, so
    // applying it again cannot move this client backward either.
    expect(versions.accept('projects', version: 5), isTrue);
  });
}
