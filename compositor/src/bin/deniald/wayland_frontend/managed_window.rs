//! The protocol boundary for managed client windows.
//!
//! XDG and Xwayland enter Denial through different protocol callbacks, but
//! they become the same Smithay [`Window`] before shell policy sees them.
//! This adapter is the only place where managed-window policy may interpret
//! backend-specific state. Everything above it consumes [`ManagedWindowFacts`]
//! and invokes backend-neutral operations.

use smithay::desktop::Window;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_output;
use smithay::utils::{Logical, Rectangle, Size};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::{
    SurfaceCachedState, ToplevelState, ToplevelSurface, XdgToplevelSurfaceData,
};
use smithay::xwayland::xwm::{WmWindowType, X11Surface};
use tracing::warn;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct ClientWindowState {
    pub(super) fullscreen: bool,
    pub(super) maximized: bool,
    pub(super) resizing: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ManagedWindowFacts {
    pub(super) client_state: ClientWindowState,
    pub(super) server_side_decorated: bool,
    pub(super) auxiliary: bool,
    pub(super) override_redirect: bool,
    pub(super) minimum_size: Size<i32, Logical>,
    pub(super) maximum_size: Size<i32, Logical>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ClientStateRequestKind {
    Maximize,
    Unmaximize,
    Fullscreen,
    Unfullscreen,
}

enum ManagedWindowProtocol<'a> {
    Xdg(&'a ToplevelSurface),
    X11(&'a X11Surface),
}

fn prepare_geometry_reassertion(
    pending: &mut ToplevelState,
    target_size: Size<i32, Logical>,
    exact: bool,
) {
    if exact {
        pending.states.unset(xdg_toplevel::State::Resizing);
        pending.states.unset(xdg_toplevel::State::Fullscreen);
        pending.states.unset(xdg_toplevel::State::Maximized);
        pending.fullscreen_output = None;
    }
    pending.size = Some(target_size);
}

/// A managed Denial client window after its protocol has been normalized.
pub(super) struct ManagedWindow<'a> {
    protocol: ManagedWindowProtocol<'a>,
}

impl<'a> ManagedWindow<'a> {
    pub(super) fn new(window: &'a Window) -> Option<Self> {
        let protocol = if let Some(toplevel) = window.toplevel() {
            ManagedWindowProtocol::Xdg(toplevel)
        } else {
            ManagedWindowProtocol::X11(window.x11_surface()?)
        };
        Some(Self { protocol })
    }

    pub(super) fn facts(&self) -> ManagedWindowFacts {
        match self.protocol {
            ManagedWindowProtocol::Xdg(toplevel) => {
                let (minimum_size, maximum_size) = with_states(toplevel.wl_surface(), |states| {
                    let mut cached = states.cached_state.get::<SurfaceCachedState>();
                    let current = cached.current();
                    (current.min_size, current.max_size)
                });
                ManagedWindowFacts {
                    client_state: ClientWindowState {
                        fullscreen: toplevel_has_state(toplevel, xdg_toplevel::State::Fullscreen),
                        maximized: toplevel_has_state(toplevel, xdg_toplevel::State::Maximized),
                        resizing: toplevel_has_state(toplevel, xdg_toplevel::State::Resizing),
                    },
                    server_side_decorated: true,
                    auxiliary: false,
                    override_redirect: false,
                    minimum_size,
                    maximum_size,
                }
            }
            ManagedWindowProtocol::X11(surface) => {
                let auxiliary = !matches!(surface.window_type(), None | Some(WmWindowType::Normal));
                let protocol_popup = matches!(
                    surface.window_type(),
                    Some(
                        WmWindowType::Combo
                            | WmWindowType::Dnd
                            | WmWindowType::DropdownMenu
                            | WmWindowType::Menu
                            | WmWindowType::Notification
                            | WmWindowType::PopupMenu
                            | WmWindowType::Tooltip
                    )
                );
                ManagedWindowFacts {
                    client_state: ClientWindowState {
                        fullscreen: surface.is_fullscreen(),
                        maximized: surface.is_maximized(),
                        resizing: false,
                    },
                    // Client motif hints do not choose Denial's frame policy.
                    // Only protocol-level popups and override-redirect surfaces
                    // are excluded from the managed toplevel frame.
                    server_side_decorated: !surface.is_override_redirect() && !protocol_popup,
                    auxiliary,
                    override_redirect: surface.is_override_redirect(),
                    minimum_size: surface.min_size().unwrap_or_else(|| Size::from((0, 0))),
                    maximum_size: surface.max_size().unwrap_or_else(|| Size::from((0, 0))),
                }
            }
        }
    }

    pub(super) fn can_store_client_restore(&self) -> bool {
        match self.protocol {
            ManagedWindowProtocol::Xdg(toplevel) => toplevel.is_initial_configure_sent(),
            ManagedWindowProtocol::X11(_) => true,
        }
    }

    /// Publishes a backend-neutral activation change. `Window::set_activated`
    /// updates common Smithay state; only XDG needs an additional configure
    /// handshake, which stays behind this protocol boundary.
    pub(super) fn prepare_activation(&self, publish: bool) {
        if !publish {
            return;
        }
        if let ManagedWindowProtocol::Xdg(toplevel) = self.protocol
            && toplevel.wl_surface().is_alive()
        {
            toplevel.send_pending_configure();
        }
    }

    /// Applies the terminal protocol state for one normalized client request.
    /// Output selection, restore ownership, layout policy, and geometry are
    /// deliberately resolved before reaching this boundary.
    pub(super) fn prepare_client_state_request(
        &self,
        request: ClientStateRequestKind,
        target_size: Option<Size<i32, Logical>>,
        fullscreen_output: Option<wl_output::WlOutput>,
    ) -> bool {
        match self.protocol {
            ManagedWindowProtocol::Xdg(toplevel) => {
                if !toplevel.wl_surface().is_alive() {
                    return false;
                }
                let changed = toplevel.with_pending_state(|pending| match request {
                    ClientStateRequestKind::Maximize => {
                        let mut changed = pending.states.set(xdg_toplevel::State::Maximized);
                        changed |= pending.states.unset(xdg_toplevel::State::Fullscreen);
                        changed |= pending.states.unset(xdg_toplevel::State::Resizing);
                        pending.fullscreen_output = None;
                        pending.size = target_size;
                        changed
                    }
                    ClientStateRequestKind::Unmaximize => {
                        let changed = pending.states.unset(xdg_toplevel::State::Maximized);
                        if changed && !pending.states.contains(xdg_toplevel::State::Fullscreen) {
                            pending.size = target_size;
                        }
                        changed
                    }
                    ClientStateRequestKind::Fullscreen => {
                        let mut changed = pending.states.set(xdg_toplevel::State::Fullscreen);
                        changed |= pending.states.unset(xdg_toplevel::State::Maximized);
                        changed |= pending.states.unset(xdg_toplevel::State::Resizing);
                        pending.fullscreen_output = fullscreen_output;
                        pending.size = target_size;
                        changed
                    }
                    ClientStateRequestKind::Unfullscreen => {
                        let changed = pending.states.unset(xdg_toplevel::State::Fullscreen);
                        if changed {
                            pending.fullscreen_output = None;
                            if !pending.states.contains(xdg_toplevel::State::Maximized) {
                                pending.size = target_size;
                            }
                        }
                        changed
                    }
                });
                if changed && toplevel.is_initial_configure_sent() {
                    toplevel.send_configure();
                }
                changed
            }
            ManagedWindowProtocol::X11(surface) => {
                let before = self.facts().client_state;
                let changed = match request {
                    ClientStateRequestKind::Maximize => {
                        if before.fullscreen
                            && let Err(error) = surface.set_fullscreen(false)
                        {
                            warn!(%error, window = surface.window_id(), "could not clear fullscreen state");
                        }
                        if !before.maximized
                            && let Err(error) = surface.set_maximized(true)
                        {
                            warn!(%error, window = surface.window_id(), "could not maximize window");
                        }
                        !before.maximized || before.fullscreen
                    }
                    ClientStateRequestKind::Unmaximize => {
                        if before.maximized
                            && let Err(error) = surface.set_maximized(false)
                        {
                            warn!(%error, window = surface.window_id(), "could not restore window");
                        }
                        before.maximized
                    }
                    ClientStateRequestKind::Fullscreen => {
                        if before.maximized
                            && let Err(error) = surface.set_maximized(false)
                        {
                            warn!(%error, window = surface.window_id(), "could not clear maximized state");
                        }
                        if !before.fullscreen
                            && let Err(error) = surface.set_fullscreen(true)
                        {
                            warn!(%error, window = surface.window_id(), "could not fullscreen window");
                        }
                        !before.fullscreen || before.maximized
                    }
                    ClientStateRequestKind::Unfullscreen => {
                        if before.fullscreen
                            && let Err(error) = surface.set_fullscreen(false)
                        {
                            warn!(%error, window = surface.window_id(), "could not leave fullscreen");
                        }
                        before.fullscreen
                    }
                };
                changed
            }
        }
    }

    /// Clears client-owned fullscreen/maximize constraints before a shell
    /// operation takes geometry ownership.
    pub(super) fn clear_geometry_constraints(&self) -> bool {
        match self.protocol {
            ManagedWindowProtocol::Xdg(toplevel) => {
                if !toplevel.wl_surface().is_alive() {
                    return false;
                }
                toplevel.with_pending_state(|pending| {
                    let fullscreen = pending.states.unset(xdg_toplevel::State::Fullscreen);
                    let maximized = pending.states.unset(xdg_toplevel::State::Maximized);
                    if fullscreen {
                        pending.fullscreen_output = None;
                    }
                    fullscreen || maximized
                })
            }
            ManagedWindowProtocol::X11(surface) => {
                let fullscreen = surface.is_fullscreen();
                let maximized = surface.is_maximized();
                if fullscreen && let Err(error) = surface.set_fullscreen(false) {
                    warn!(%error, window = surface.window_id(), "could not clear X11 fullscreen for shell geometry");
                }
                if maximized && let Err(error) = surface.set_maximized(false) {
                    warn!(%error, window = surface.window_id(), "could not clear X11 maximized state for shell geometry");
                }
                fullscreen || maximized
            }
        }
    }

    /// Performs the protocol handshake for a shell-owned geometry target.
    /// The target itself is applied once by `set_window_geometry_target`.
    pub(super) fn prepare_shell_geometry(&self, target: Rectangle<i32, Logical>) {
        if let ManagedWindowProtocol::Xdg(toplevel) = self.protocol {
            toplevel.with_pending_state(|pending| {
                pending.states.unset(xdg_toplevel::State::Resizing);
                pending.size = Some(target.size);
            });
            toplevel.send_pending_configure();
        }
    }

    /// Performs the protocol handshake for a compositor-owned tile target.
    pub(super) fn prepare_tiled_geometry(
        &self,
        target: Rectangle<i32, Logical>,
        force_resize: bool,
    ) {
        match self.protocol {
            ManagedWindowProtocol::Xdg(toplevel) => {
                let client_maximized = toplevel_has_state(toplevel, xdg_toplevel::State::Maximized);
                toplevel.with_pending_state(|pending| {
                    pending.states.unset(xdg_toplevel::State::Resizing);
                    pending.states.unset(xdg_toplevel::State::Maximized);
                    pending.size = Some(target.size);
                });
                if toplevel.is_initial_configure_sent() && (client_maximized || force_resize) {
                    // Force a new serial when a prior configure already cached
                    // this target but the client still presents its old buffer.
                    toplevel.send_configure();
                }
            }
            ManagedWindowProtocol::X11(surface) => {
                if surface.is_maximized()
                    && let Err(error) = surface.set_maximized(false)
                {
                    warn!(%error, window = surface.window_id(), "could not clear maximize state for tiled window");
                }
            }
        }
    }

    pub(super) fn prepare_restore_size(&self, size: Size<i32, Logical>, force: bool) {
        if let ManagedWindowProtocol::Xdg(toplevel) = self.protocol {
            toplevel.with_pending_state(|pending| pending.size = Some(size));
            if toplevel.is_initial_configure_sent() {
                if force {
                    toplevel.send_configure();
                } else {
                    toplevel.send_pending_configure();
                }
            }
        }
    }

    pub(super) fn prepare_interactive_resize(&self, size: Size<i32, Logical>, finished: bool) {
        if let ManagedWindowProtocol::Xdg(toplevel) = self.protocol
            && toplevel.wl_surface().is_alive()
        {
            toplevel.with_pending_state(|pending| {
                if finished {
                    pending.states.unset(xdg_toplevel::State::Resizing);
                } else {
                    pending.states.set(xdg_toplevel::State::Resizing);
                }
                pending.size = Some(size);
            });
            toplevel.send_pending_configure();
        }
    }

    pub(super) fn accepts_interactive_resize_updates(&self) -> bool {
        match self.protocol {
            ManagedWindowProtocol::Xdg(toplevel) => {
                toplevel.wl_surface().is_alive()
                    && toplevel.with_pending_state(|pending| {
                        pending.states.contains(xdg_toplevel::State::Resizing)
                            && !pending.states.contains(xdg_toplevel::State::Fullscreen)
                            && !pending.states.contains(xdg_toplevel::State::Maximized)
                    })
            }
            ManagedWindowProtocol::X11(surface) => {
                !surface.is_override_redirect()
                    && !surface.is_fullscreen()
                    && !surface.is_maximized()
            }
        }
    }

    /// Sends the backend's terminal geometry operation. XDG size negotiation
    /// is prepared by the higher-level operation that owns its configure
    /// serial; X11 receives its ConfigureWindow here.
    pub(super) fn prepare_geometry_target(&self, target: Rectangle<i32, Logical>, force: bool) {
        if let ManagedWindowProtocol::X11(surface) = self.protocol
            && !surface.is_override_redirect()
            && (force || surface.last_configure() != target)
            && let Err(error) = surface.configure(target)
        {
            warn!(%error, window = surface.window_id(), "could not configure managed window geometry");
        }
    }

    /// Reassert a compositor-owned target after a client committed a
    /// different buffer. This is one normalized operation; only the terminal
    /// protocol handshake differs.
    pub(super) fn reassert_geometry_target(&self, target: Rectangle<i32, Logical>, exact: bool) {
        match self.protocol {
            ManagedWindowProtocol::Xdg(toplevel) => {
                toplevel.with_pending_state(|pending| {
                    prepare_geometry_reassertion(pending, target.size, exact);
                });
                if toplevel.is_initial_configure_sent() {
                    toplevel.send_configure();
                }
            }
            ManagedWindowProtocol::X11(_) => self.prepare_geometry_target(target, true),
        }
    }

    pub(super) fn close(&self) -> bool {
        match self.protocol {
            ManagedWindowProtocol::Xdg(toplevel) => {
                toplevel.send_close();
                true
            }
            ManagedWindowProtocol::X11(surface) => {
                if let Err(error) = surface.close() {
                    warn!(%error, "could not close managed window");
                    false
                } else {
                    true
                }
            }
        }
    }

    /// Acknowledges Denial's common minimized visibility state. X11 remains
    /// mapped while minimized so its live Flutter texture cannot go stale.
    pub(super) fn prepare_minimized(&self, minimized: bool) {
        match self.protocol {
            ManagedWindowProtocol::Xdg(toplevel) => {
                let changed = toplevel.with_pending_state(|pending| {
                    if minimized {
                        pending.states.set(xdg_toplevel::State::Suspended)
                    } else {
                        pending.states.unset(xdg_toplevel::State::Suspended)
                    }
                });
                if changed {
                    toplevel.send_pending_configure();
                }
            }
            ManagedWindowProtocol::X11(surface) if !minimized => {
                if let Err(error) = surface.set_hidden(false) {
                    warn!(%error, window = surface.window_id(), "could not restore managed window");
                }
            }
            ManagedWindowProtocol::X11(_) => {}
        }
    }
}

pub(super) fn toplevel_has_state(
    surface: &ToplevelSurface,
    xdg_state: xdg_toplevel::State,
) -> bool {
    if !surface.wl_surface().is_alive() {
        return false;
    }
    with_states(surface.wl_surface(), |states| {
        let Some(attributes) = states.data_map.get::<XdgToplevelSurfaceData>() else {
            return false;
        };
        let attributes = attributes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        attributes
            .server_pending
            .clone()
            .unwrap_or_else(|| attributes.current_server_state())
            .states
            .contains(xdg_state)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_geometry_reassertion_preserves_live_resize_state() {
        let mut pending = ToplevelState::default();
        pending.states.set(xdg_toplevel::State::Resizing);

        prepare_geometry_reassertion(&mut pending, Size::from((900, 700)), false);

        assert!(pending.states.contains(xdg_toplevel::State::Resizing));
        assert_eq!(pending.size, Some(Size::from((900, 700))));
    }

    #[test]
    fn exact_geometry_reassertion_clears_client_constraints() {
        let mut pending = ToplevelState::default();
        pending.states.set(xdg_toplevel::State::Resizing);
        pending.states.set(xdg_toplevel::State::Fullscreen);
        pending.states.set(xdg_toplevel::State::Maximized);

        prepare_geometry_reassertion(&mut pending, Size::from((1080, 1920)), true);

        assert!(!pending.states.contains(xdg_toplevel::State::Resizing));
        assert!(!pending.states.contains(xdg_toplevel::State::Fullscreen));
        assert!(!pending.states.contains(xdg_toplevel::State::Maximized));
        assert_eq!(pending.size, Some(Size::from((1080, 1920))));
    }
}
