import 'package:denial_dart_shell/src/desktop/desktop_workspace.dart';
import 'package:denial_dart_shell/src/models/denial_window.dart';
import 'package:denial_dart_shell/src/models/denial_window_event.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';

DenialWindow nativeWindow({
  required String objectKind,
  required Rect geometry,
  required bool fullscreen,
}) {
  return DenialWindow(
    objectId: 7,
    objectKind: objectKind,
    surfaceId: 17,
    windowId: 27,
    textureId: 37,
    title: 'Protocol client',
    appId: 'protocol.client',
    width: geometry.width.round(),
    height: geometry.height.round(),
    surfaceX: 0,
    surfaceY: 0,
    surfaceWidth: geometry.width,
    surfaceHeight: geometry.height,
    textureSourceX: 0,
    textureSourceY: 0,
    textureSourceWidth: geometry.width,
    textureSourceHeight: geometry.height,
    geometryX: geometry.left,
    geometryY: geometry.top,
    geometryWidth: geometry.width,
    geometryHeight: geometry.height,
    monitorId: 1,
    fullscreen: fullscreen,
    transform: 0,
    scale120: 120,
  );
}

void main() {
  for (final objectKind in <String>['xdg', 'x11']) {
    test(
      '$objectKind startup-fullscreen action cannot block a later layout split',
      () {
        final container = ProviderContainer();
        addTearDown(container.dispose);
        final workspace = container.read(desktopWorkspaceProvider.notifier);
        const viewSize = Size(1920, 1080);
        const fullscreenGeometry = Rect.fromLTWH(0, 0, 1920, 1080);
        final fullscreen = nativeWindow(
          objectKind: objectKind,
          geometry: fullscreenGeometry,
          fullscreen: true,
        );

        workspace.syncWindows(
          <DenialWindow>[fullscreen],
          viewSize,
          1,
          snapshotSequence: 10,
        );

        expect(
          workspace.applyFlutterOwnedWindowAction(
            fullscreen,
            DenialWindowAction.restore,
            maximizeBounds: fullscreenGeometry,
            fullscreenBounds: fullscreenGeometry,
          ),
          isFalse,
        );
        expect(
          container.read(desktopWorkspaceProvider).placements[7]!.frame,
          fullscreenGeometry,
        );

        const tileGeometry = Rect.fromLTWH(10, 30, 900, 1000);
        workspace.syncWindows(
          <DenialWindow>[
            nativeWindow(
              objectKind: objectKind,
              geometry: tileGeometry,
              fullscreen: false,
            ),
          ],
          viewSize,
          1,
          snapshotSequence: 11,
        );

        final placement = container
            .read(desktopWorkspaceProvider)
            .placements[7]!;
        expect(placement.fullscreen, isFalse);
        expect(placement.contentRect, tileGeometry);
      },
    );
  }
}
