import 'dart:math' as math;
import 'dart:ui' as ui;

import 'package:flutter/rendering.dart' show RenderRepaintBoundary;
import 'package:flutter/widgets.dart';

import '../theme/shell_theme.dart';
import 'desktop_window_render_telemetry.dart';
import 'desktop_workspace.dart';

Color desktopWindowBorderColor({
  required bool pinned,
  required bool active,
  required ShellThemeData theme,
  required Color inactiveColor,
}) {
  if (pinned) {
    return theme.accentPalette.container;
  }
  if (active && theme.focusedWindowBorderEnabled) {
    return theme.accent;
  }
  return inactiveColor;
}

/// Keeps the static server-side decoration isolated from the live client
/// texture and from the stateful focus border.
///
/// Only the shadow picture is marked complex. Applying the hint to a single
/// [CustomPaint] with both a background and foreground painter would also
/// force a full-window cache for the inexpensive frame and focus border.
class DesktopWindowFrameLayers extends StatelessWidget {
  const DesktopWindowFrameLayers({
    required this.windowId,
    required this.devicePixelRatio,
    required this.radius,
    required this.frameColor,
    required this.borderColor,
    required this.child,
    super.key,
  });

  final int windowId;
  final double devicePixelRatio;
  final double radius;
  final Color frameColor;
  final Color borderColor;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    return Stack(
      fit: StackFit.expand,
      clipBehavior: Clip.none,
      children: [
        DesktopWindowRepaintBoundary(
          outset: DesktopWindowShadowPainter.shadowOutset,
          child: IgnorePointer(
            child: CustomPaint(
              painter: DesktopWindowShadowPainter(
                windowId: windowId,
                radius: radius,
                shadowColor: context.shellColors.shadow,
              ),
              isComplex: true,
              willChange: false,
            ),
          ),
        ),
        child,
        IgnorePointer(
          child: CustomPaint(
            painter: DesktopWindowFramePainter(
              windowId: windowId,
              devicePixelRatio: devicePixelRatio,
              radius: radius,
              frameColor: frameColor,
              borderColor: borderColor,
            ),
          ),
        ),
      ],
    );
  }
}

/// A retained layer whose damage bounds include deliberate child overdraw.
///
/// [DesktopWindowShadowPainter] paints its shadow outside the window's layout
/// rectangle. A normal [RepaintBoundary] would advertise only that rectangle,
/// leaving the outsetting shadow outside partial-repaint damage.
class DesktopWindowRepaintBoundary extends RepaintBoundary {
  const DesktopWindowRepaintBoundary({
    required this.outset,
    super.child,
    super.key,
  });

  final double outset;

  @override
  RenderDesktopWindowRepaintBoundary createRenderObject(BuildContext context) {
    return RenderDesktopWindowRepaintBoundary(outset);
  }

  @override
  void updateRenderObject(
    BuildContext context,
    covariant RenderDesktopWindowRepaintBoundary renderObject,
  ) {
    renderObject.outset = outset;
  }
}

class RenderDesktopWindowRepaintBoundary extends RenderRepaintBoundary {
  RenderDesktopWindowRepaintBoundary(this._outset) : assert(_outset >= 0);

  double _outset;

  set outset(double value) {
    assert(value >= 0);
    if (_outset == value) {
      return;
    }
    _outset = value;
    markNeedsPaint();
  }

  @override
  Rect get paintBounds => super.paintBounds.inflate(_outset);
}

/// Paints the cached shadow behind a decorated client surface.
class DesktopWindowShadowPainter extends CustomPainter {
  const DesktopWindowShadowPainter({
    this.windowId = 0,
    required this.radius,
    required this.shadowColor,
  });

  final int windowId;
  final double radius;
  final Color shadowColor;

  static const double shadowOutset = 64;

  @override
  void paint(Canvas canvas, Size size) {
    DesktopWindowRenderTelemetry.recordShadowPaint(windowId, size);
    if (size.isEmpty) {
      return;
    }

    final frame = Offset.zero & size;
    final frameShape = RRect.fromRectAndRadius(frame, Radius.circular(radius));
    final outsideFrame = Path()
      ..fillType = PathFillType.evenOdd
      ..addRect(frame.inflate(shadowOutset))
      ..addRRect(frameShape);
    final shadowRect = frame.shift(const Offset(0, 12)).inflate(2);
    final shadowPaint = Paint()
      ..color = shadowColor
      ..maskFilter = const MaskFilter.blur(BlurStyle.normal, 16.5);

    canvas
      ..save()
      ..clipPath(outsideFrame)
      ..drawRRect(
        RRect.fromRectAndRadius(shadowRect, Radius.circular(radius * 1.25)),
        shadowPaint,
      )
      ..restore();
  }

  @override
  bool shouldRepaint(covariant DesktopWindowShadowPainter oldDelegate) {
    return windowId != oldDelegate.windowId ||
        radius != oldDelegate.radius ||
        shadowColor != oldDelegate.shadowColor;
  }
}

typedef DesktopRoundedFrameGeometry = ({
  double borderThickness,
  double edgeHalfWidth,
  double frameThickness,
  double innerRadius,
  double outerRadius,
  double shaderRadius,
});

/// Resolves the physical-pixel coverage geometry used by the window frame.
///
/// The complete outer transition is one physical pixel wide. Capping each
/// half at half the frame thickness keeps the inner and outer transitions from
/// crossing when an unusually small frame is requested.
DesktopRoundedFrameGeometry desktopRoundedFrameGeometry({
  required Size size,
  required double radius,
  required double frameThickness,
  required double borderThickness,
  required double devicePixelRatio,
}) {
  final ratio = devicePixelRatio.isFinite && devicePixelRatio > 0.0
      ? devicePixelRatio
      : 1.0;
  final shortestSide = math.max(0.0, math.min(size.width, size.height));
  final resolvedRadius = math.min(math.max(0.0, radius), shortestSide / 2.0);
  final resolvedFrameThickness = math.min(
    math.max(0.0, frameThickness),
    shortestSide / 2.0,
  );
  final resolvedBorderThickness = math.min(
    math.max(0.0, borderThickness),
    resolvedFrameThickness,
  );
  final edgeHalfWidth = math.min(
    resolvedRadius,
    math.min(0.5 / ratio, resolvedFrameThickness / 2.0),
  );
  return (
    borderThickness: resolvedBorderThickness,
    edgeHalfWidth: edgeHalfWidth,
    frameThickness: resolvedFrameThickness,
    innerRadius: math.max(0.0, resolvedRadius - resolvedFrameThickness),
    outerRadius: resolvedRadius,
    shaderRadius: resolvedRadius + edgeHalfWidth,
  );
}

/// Paints the opaque frame and its stateful border with an analytic one-pixel
/// coverage fringe.
///
/// Impeller can therefore render the rounded silhouette smoothly even when
/// the external GLES framebuffer is single-sampled. Only four radius-sized
/// gradient patches are shaded; no save-layer or output-sized attachment is
/// allocated. The center stays clear so client-provided per-pixel alpha keeps
/// its existing compositing semantics.
class DesktopWindowFramePainter extends CustomPainter {
  const DesktopWindowFramePainter({
    this.windowId = 0,
    required this.devicePixelRatio,
    required this.radius,
    required this.frameColor,
    this.borderColor,
  });

  final int windowId;
  final double devicePixelRatio;
  final double radius;
  final Color frameColor;
  final Color? borderColor;

  @override
  void paint(Canvas canvas, Size size) {
    DesktopWindowRenderTelemetry.recordBorderPaint(windowId, size);
    if (size.isEmpty) {
      return;
    }

    _paintRoundedFrame(
      canvas,
      Offset.zero & size,
      radius: radius,
      frameThickness: DesktopMetrics.frameBorder,
      borderThickness: 1.0 / _resolvedDevicePixelRatio,
      devicePixelRatio: _resolvedDevicePixelRatio,
      frameColor: frameColor,
      borderColor: borderColor,
    );
  }

  double get _resolvedDevicePixelRatio =>
      devicePixelRatio.isFinite && devicePixelRatio > 0.0
      ? devicePixelRatio
      : 1.0;

  @override
  bool shouldRepaint(covariant DesktopWindowFramePainter oldDelegate) {
    return windowId != oldDelegate.windowId ||
        devicePixelRatio != oldDelegate.devicePixelRatio ||
        radius != oldDelegate.radius ||
        frameColor != oldDelegate.frameColor ||
        borderColor != oldDelegate.borderColor;
  }
}

void _paintRoundedFrame(
  Canvas canvas,
  Rect frame, {
  required double radius,
  required double frameThickness,
  required double borderThickness,
  required double devicePixelRatio,
  required Color frameColor,
  required Color? borderColor,
}) {
  final geometry = desktopRoundedFrameGeometry(
    size: frame.size,
    radius: radius,
    frameThickness: frameThickness,
    borderThickness: borderThickness,
    devicePixelRatio: devicePixelRatio,
  );
  if (geometry.frameThickness <= 0.0) {
    return;
  }

  final compositeBorderColor = borderColor == null
      ? frameColor
      : Color.alphaBlend(borderColor, frameColor);
  if (geometry.outerRadius <= 0.0) {
    _paintSquareFrame(
      canvas,
      frame,
      frameThickness: geometry.frameThickness,
      borderThickness: geometry.borderThickness,
      frameColor: frameColor,
      borderColor: compositeBorderColor,
    );
    return;
  }

  _paintFrameStrips(
    canvas,
    frame,
    radius: geometry.outerRadius,
    frameThickness: geometry.frameThickness,
    borderThickness: geometry.borderThickness,
    frameColor: frameColor,
    borderColor: compositeBorderColor,
  );
  _paintFrameCorners(
    canvas,
    frame,
    geometry: geometry,
    frameColor: frameColor,
    borderColor: compositeBorderColor,
  );
}

void _paintSquareFrame(
  Canvas canvas,
  Rect frame, {
  required double frameThickness,
  required double borderThickness,
  required Color frameColor,
  required Color borderColor,
}) {
  canvas.drawDRRect(
    RRect.fromRectAndRadius(frame, Radius.zero),
    RRect.fromRectAndRadius(frame.deflate(frameThickness), Radius.zero),
    Paint()
      ..color = frameColor
      ..isAntiAlias = false,
  );
  _paintRects(canvas, _edgeRects(frame, borderThickness), borderColor);
}

void _paintFrameStrips(
  Canvas canvas,
  Rect frame, {
  required double radius,
  required double frameThickness,
  required double borderThickness,
  required Color frameColor,
  required Color borderColor,
}) {
  _paintRects(canvas, <Rect>[
    Rect.fromLTRB(
      frame.left + radius,
      frame.top,
      frame.right - radius,
      frame.top + frameThickness,
    ),
    Rect.fromLTRB(
      frame.left + radius,
      frame.bottom - frameThickness,
      frame.right - radius,
      frame.bottom,
    ),
    Rect.fromLTRB(
      frame.left,
      frame.top + radius,
      frame.left + frameThickness,
      frame.bottom - radius,
    ),
    Rect.fromLTRB(
      frame.right - frameThickness,
      frame.top + radius,
      frame.right,
      frame.bottom - radius,
    ),
  ], frameColor);
  _paintRects(canvas, <Rect>[
    ..._edgeRects(
      Rect.fromLTRB(
        frame.left + radius,
        frame.top,
        frame.right - radius,
        frame.bottom,
      ),
      borderThickness,
      vertical: false,
    ),
    ..._edgeRects(
      Rect.fromLTRB(
        frame.left,
        frame.top + radius,
        frame.right,
        frame.bottom - radius,
      ),
      borderThickness,
      horizontal: false,
    ),
  ], borderColor);
}

List<Rect> _edgeRects(
  Rect frame,
  double thickness, {
  bool horizontal = true,
  bool vertical = true,
}) {
  if (thickness <= 0.0 || frame.isEmpty) {
    return const <Rect>[];
  }
  final rects = <Rect>[];
  if (horizontal) {
    rects
      ..add(
        Rect.fromLTRB(
          frame.left,
          frame.top,
          frame.right,
          frame.top + thickness,
        ),
      )
      ..add(
        Rect.fromLTRB(
          frame.left,
          frame.bottom - thickness,
          frame.right,
          frame.bottom,
        ),
      );
  }
  if (vertical) {
    rects
      ..add(
        Rect.fromLTRB(
          frame.left,
          frame.top,
          frame.left + thickness,
          frame.bottom,
        ),
      )
      ..add(
        Rect.fromLTRB(
          frame.right - thickness,
          frame.top,
          frame.right,
          frame.bottom,
        ),
      );
  }
  return rects;
}

void _paintRects(Canvas canvas, Iterable<Rect> rects, Color color) {
  final path = Path();
  for (final rect in rects) {
    if (!rect.isEmpty) {
      path.addRect(rect);
    }
  }
  canvas.drawPath(
    path,
    Paint()
      ..color = color
      ..isAntiAlias = false,
  );
}

void _paintFrameCorners(
  Canvas canvas,
  Rect frame, {
  required DesktopRoundedFrameGeometry geometry,
  required Color frameColor,
  required Color borderColor,
}) {
  final centers = <Offset>[
    Offset(frame.left + geometry.outerRadius, frame.top + geometry.outerRadius),
    Offset(
      frame.right - geometry.outerRadius,
      frame.top + geometry.outerRadius,
    ),
    Offset(
      frame.right - geometry.outerRadius,
      frame.bottom - geometry.outerRadius,
    ),
    Offset(
      frame.left + geometry.outerRadius,
      frame.bottom - geometry.outerRadius,
    ),
  ];
  final cornerRects = <Rect>[
    Rect.fromLTRB(frame.left, frame.top, centers[0].dx, centers[0].dy),
    Rect.fromLTRB(centers[1].dx, frame.top, frame.right, centers[1].dy),
    Rect.fromLTRB(centers[2].dx, centers[2].dy, frame.right, frame.bottom),
    Rect.fromLTRB(frame.left, centers[3].dy, centers[3].dx, frame.bottom),
  ];
  final profile = _frameCornerProfile(
    geometry,
    frameColor: frameColor,
    borderColor: borderColor,
  );
  final paint = Paint()..isAntiAlias = false;
  for (var index = 0; index < centers.length; index++) {
    paint.shader = ui.Gradient.radial(
      centers[index],
      geometry.shaderRadius,
      profile.colors,
      profile.stops,
    );
    canvas.drawRect(cornerRects[index], paint);
  }
}

({List<Color> colors, List<double> stops}) _frameCornerProfile(
  DesktopRoundedFrameGeometry geometry, {
  required Color frameColor,
  required Color borderColor,
}) {
  final transparentFrame = frameColor.withValues(alpha: 0.0);
  final transparentBorder = borderColor.withValues(alpha: 0.0);
  final shaderRadius = geometry.shaderRadius;
  final innerStart = math.max(
    0.0,
    geometry.innerRadius - geometry.edgeHalfWidth,
  );
  final innerEnd = geometry.innerRadius + geometry.edgeHalfWidth;
  final outerStart = geometry.outerRadius - geometry.edgeHalfWidth;
  final borderCenter = geometry.outerRadius - geometry.borderThickness;
  final borderStart = math.max(innerEnd, borderCenter - geometry.edgeHalfWidth);
  final borderEnd = math.max(
    borderStart,
    math.min(outerStart, borderCenter + geometry.edgeHalfWidth),
  );
  final resolvedOuterStart = math.max(borderEnd, outerStart);
  final fillsCenter = geometry.innerRadius <= 0.0;

  return (
    colors: <Color>[
      fillsCenter ? frameColor : transparentFrame,
      frameColor,
      frameColor,
      borderColor,
      borderColor,
      transparentBorder,
    ],
    stops: <double>[
      innerStart / shaderRadius,
      innerEnd / shaderRadius,
      borderStart / shaderRadius,
      borderEnd / shaderRadius,
      resolvedOuterStart / shaderRadius,
      1.0,
    ],
  );
}
