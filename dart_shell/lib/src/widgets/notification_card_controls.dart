part of 'notification_banner.dart';

class _NotificationActivator extends StatefulWidget {
  const _NotificationActivator({
    required this.semanticLabel,
    required this.onActivate,
    required this.borderRadius,
    required this.child,
  });

  final String semanticLabel;
  final VoidCallback onActivate;
  final BorderRadius borderRadius;
  final Widget child;

  @override
  State<_NotificationActivator> createState() => _NotificationActivatorState();
}

class _NotificationActivatorState extends State<_NotificationActivator> {
  bool _focused = false;

  @override
  Widget build(BuildContext context) {
    return FocusableActionDetector(
      mouseCursor: SystemMouseCursors.click,
      onShowFocusHighlight: (focused) => setState(() => _focused = focused),
      shortcuts: const <ShortcutActivator, Intent>{
        SingleActivator(LogicalKeyboardKey.enter): ActivateIntent(),
        SingleActivator(LogicalKeyboardKey.space): ActivateIntent(),
      },
      actions: <Type, Action<Intent>>{
        ActivateIntent: CallbackAction<ActivateIntent>(
          onInvoke: (_) {
            widget.onActivate();
            return null;
          },
        ),
      },
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onTap: widget.onActivate,
        child: DecoratedBox(
          decoration: BoxDecoration(
            borderRadius: widget.borderRadius,
            border: _focused
                ? Border.all(color: ShellTheme.of(context).accent, width: 1.5)
                : null,
          ),
          child: widget.child,
        ),
      ),
    );
  }
}

class _NotificationActionButton extends StatefulWidget {
  const _NotificationActionButton({
    required this.label,
    required this.onPressed,
    this.mobile = false,
    this.fontSize = 12,
    this.textColor,
    this.visualScale = 1,
  });

  final String label;
  final bool mobile;
  final double fontSize;
  final Color? textColor;
  final double visualScale;
  final VoidCallback onPressed;

  @override
  State<_NotificationActionButton> createState() =>
      _NotificationActionButtonState();
}

class _NotificationActionButtonState extends State<_NotificationActionButton> {
  bool _hovered = false;
  bool _focused = false;

  @override
  Widget build(BuildContext context) {
    return Semantics(
      button: true,
      label: widget.label,
      child: FocusableActionDetector(
        mouseCursor: SystemMouseCursors.click,
        onShowHoverHighlight: (hovered) => setState(() => _hovered = hovered),
        onShowFocusHighlight: (focused) => setState(() => _focused = focused),
        shortcuts: const <ShortcutActivator, Intent>{
          SingleActivator(LogicalKeyboardKey.enter): ActivateIntent(),
          SingleActivator(LogicalKeyboardKey.space): ActivateIntent(),
        },
        actions: <Type, Action<Intent>>{
          ActivateIntent: CallbackAction<ActivateIntent>(
            onInvoke: (_) {
              widget.onPressed();
              return null;
            },
          ),
        },
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: widget.onPressed,
          child: AnimatedContainer(
            duration: MediaQuery.disableAnimationsOf(context)
                ? Duration.zero
                : Motion.pill,
            curve: Motion.standard,
            constraints: BoxConstraints(
              minWidth:
                  MobileNotificationMetrics.actionMinimumWidth *
                  widget.visualScale,
            ),
            padding: EdgeInsets.symmetric(
              horizontal:
                  MobileNotificationMetrics.actionHorizontalInset *
                  widget.visualScale,
            ),
            alignment: Alignment.center,
            decoration: BoxDecoration(
              color: _hovered || _focused
                  ? context.shellTheme.accentPalette.container
                  : context.shellColors.surfaceContainerHighest,
              borderRadius: context.shellTheme.borderRadius(
                MobileNotificationMetrics.actionRadius * widget.visualScale,
              ),
              border: _focused
                  ? Border.all(color: ShellTheme.of(context).accent)
                  : widget.mobile &&
                        context.shellTheme.transparencyMode ==
                            ShellTransparencyMode.glass
                  ? null
                  : Border.all(color: context.shellColors.hairlineSoft),
            ),
            child: Text(
              widget.label,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: ShellText.cardTitle.copyWith(
                fontSize: widget.fontSize,
                color: widget.textColor,
                fontWeight: widget.textColor != null
                    ? FontWeight.w700
                    : FontWeight.w600,
              ),
            ),
          ),
        ),
      ),
    );
  }
}

class _NotificationIconButton extends StatefulWidget {
  const _NotificationIconButton({
    required this.label,
    required this.icon,
    required this.onPressed,
    this.visualScale = 1,
  });

  final String label;
  final IconData icon;
  final VoidCallback onPressed;
  final double visualScale;

  @override
  State<_NotificationIconButton> createState() =>
      _NotificationIconButtonState();
}

class _NotificationIconButtonState extends State<_NotificationIconButton> {
  bool _hovered = false;
  bool _focused = false;

  @override
  Widget build(BuildContext context) {
    return Semantics(
      button: true,
      label: widget.label,
      child: FocusableActionDetector(
        mouseCursor: SystemMouseCursors.click,
        onShowHoverHighlight: (hovered) => setState(() => _hovered = hovered),
        onShowFocusHighlight: (focused) => setState(() => _focused = focused),
        shortcuts: const <ShortcutActivator, Intent>{
          SingleActivator(LogicalKeyboardKey.enter): ActivateIntent(),
          SingleActivator(LogicalKeyboardKey.space): ActivateIntent(),
        },
        actions: <Type, Action<Intent>>{
          ActivateIntent: CallbackAction<ActivateIntent>(
            onInvoke: (_) {
              widget.onPressed();
              return null;
            },
          ),
        },
        child: GestureDetector(
          behavior: HitTestBehavior.opaque,
          onTap: widget.onPressed,
          child: AnimatedContainer(
            duration: MediaQuery.disableAnimationsOf(context)
                ? Duration.zero
                : Motion.pill,
            width: MobileNotificationMetrics.dismissExtent * widget.visualScale,
            height:
                MobileNotificationMetrics.dismissExtent * widget.visualScale,
            decoration: BoxDecoration(
              color: _hovered || _focused
                  ? context.shellColors.surfaceContainerHighest
                  : ShellMediaColors.transparentDark,
              borderRadius: context.shellTheme.borderRadius(
                MobileNotificationMetrics.dismissRadius * widget.visualScale,
              ),
              border: _focused
                  ? Border.all(color: ShellTheme.of(context).accent)
                  : null,
            ),
            child: Icon(
              widget.icon,
              size: MobileNotificationMetrics.dismissIcon * widget.visualScale,
              color: context.shellColors.textSecondary,
            ),
          ),
        ),
      ),
    );
  }
}

Offset _notificationEntryOffset(ShellPopupAnchor anchor) {
  if (anchor.vertical != 0) {
    return Offset(0, anchor.vertical.toDouble());
  }
  if (anchor.horizontal != 0) {
    return Offset(anchor.horizontal.toDouble(), 0);
  }
  return const Offset(0, -1);
}

String notificationAppName(
  DesktopNotification notification,
  AppLocalizations l10n,
) {
  if (notification.appName.isNotEmpty) {
    return notification.appName;
  }
  if (notification.desktopEntry.isNotEmpty) {
    return notification.desktopEntry;
  }
  return l10n.notificationGeneric;
}

String plainNotificationBody(String value) {
  return value
      .replaceAll(RegExp(r'<\s*br\s*/?\s*>', caseSensitive: false), '\n')
      .replaceAll(RegExp(r'<[^>]*>'), '')
      .replaceAll('&lt;', '<')
      .replaceAll('&gt;', '>')
      .replaceAll('&quot;', '"')
      .replaceAll('&apos;', "'")
      .replaceAll('&amp;', '&')
      .trim();
}
