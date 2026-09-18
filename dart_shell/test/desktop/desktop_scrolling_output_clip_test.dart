import 'package:denial_dart_shell/src/desktop/desktop_workspace.dart';
import 'package:denial_dart_shell/src/settings/shell_settings.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  const output = Rect.fromLTWH(0, 0, 1920, 1080);

  test('scrolling tiles are clipped to their output', () {
    expect(
      desktopScrollingOutputClip(
        windowLayout: DesktopWindowLayout.scrolling,
        pinned: false,
        transformed: false,
        outputRect: output,
      ),
      output,
    );
  });

  test('pinned scrolling windows remain floating and unclipped', () {
    expect(
      desktopScrollingOutputClip(
        windowLayout: DesktopWindowLayout.scrolling,
        pinned: true,
        transformed: false,
        outputRect: output,
      ),
      isNull,
    );
  });

  test('transformed and non-scrolling windows remain unclipped', () {
    expect(
      desktopScrollingOutputClip(
        windowLayout: DesktopWindowLayout.scrolling,
        pinned: false,
        transformed: true,
        outputRect: output,
      ),
      isNull,
    );
    expect(
      desktopScrollingOutputClip(
        windowLayout: DesktopWindowLayout.dwindle,
        pinned: false,
        transformed: false,
        outputRect: output,
      ),
      isNull,
    );
  });
}
