import 'package:denial_dart_shell/src/desktop/desktop_input_layout_publisher.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test('clips a client input region and maps its source coordinates', () {
    final clipped = desktopClipInputGeometryToRect(
      rect: const Rect.fromLTWH(50, 20, 200, 100),
      sourceRect: const Rect.fromLTWH(10, 5, 400, 200),
      clipRect: const Rect.fromLTWH(100, 0, 100, 200),
    );

    expect(clipped?.rect, const Rect.fromLTWH(100, 20, 100, 100));
    expect(clipped?.sourceRect, const Rect.fromLTWH(110, 5, 200, 200));
  });

  test('drops a client input region outside its output', () {
    expect(
      desktopClipInputGeometryToRect(
        rect: const Rect.fromLTWH(0, 0, 50, 50),
        sourceRect: const Rect.fromLTWH(0, 0, 50, 50),
        clipRect: const Rect.fromLTWH(100, 0, 100, 100),
      ),
      isNull,
    );
  });
}
