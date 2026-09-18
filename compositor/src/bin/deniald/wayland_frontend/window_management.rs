use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_output;
use smithay::utils::{Logical, Rectangle, Size};
#[cfg(feature = "flutter")]
use smithay::utils::{Point, SERIAL_COUNTER};
#[cfg(feature = "flutter")]
use smithay::wayland::seat::WaylandFocus;
use tracing::warn;

#[cfg(feature = "flutter")]
use super::super::PendingWindowEvent;
use super::super::RuntimeState;
use super::super::window_grab::constrain_dimension;
#[cfg(feature = "flutter")]
use super::super::window_layout::LayoutDirection;
#[cfg(feature = "flutter")]
use super::super::window_placement_store::RestoredWindowPlacement;
#[cfg(feature = "flutter")]
use super::super::wire::{
    WindowAction, WindowCommand, WindowGeometry, WindowPlacementChange, WindowPlacementPhase,
};
use super::WindowGeometryAuthority;
#[cfg(feature = "flutter")]
use super::clamp_window_geometry;
#[cfg(feature = "flutter")]
use super::focus::clear_keyboard_focus;
use super::focus::request_keyboard_focus;
use super::managed_window::{ClientStateRequestKind, ManagedWindow};

fn bound_geometry_size(mut geometry: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    geometry.size = Size::from((
        constrain_dimension(geometry.size.w, 1, 0),
        constrain_dimension(geometry.size.h, 1, 0),
    ));
    geometry
}

#[cfg(feature = "flutter")]
fn configured_window_size(
    requested: Size<i32, Logical>,
    minimum: Size<i32, Logical>,
    maximum: Size<i32, Logical>,
    compositor_owned: bool,
) -> Size<i32, Logical> {
    if compositor_owned {
        requested
    } else {
        Size::from((
            constrain_dimension(requested.w, minimum.w, maximum.w),
            constrain_dimension(requested.h, minimum.h, maximum.h),
        ))
    }
}

// Must match DesktopMetrics.frameBorder in the embedded shell.
pub(super) const SHELL_FRAME_BORDER: i32 = 1;

#[cfg(feature = "flutter")]
pub(super) fn shell_draws_server_frame(window: &Window) -> bool {
    ManagedWindow::new(window).is_some_and(|window| window.facts().server_side_decorated)
}

#[cfg(feature = "flutter")]
pub(super) fn shell_content_geometry(
    mut frame: Rectangle<i32, Logical>,
    server_side_decorated: bool,
) -> Rectangle<i32, Logical> {
    if server_side_decorated
        && frame.size.w > SHELL_FRAME_BORDER * 2
        && frame.size.h > SHELL_FRAME_BORDER * 2
    {
        frame.loc.x += SHELL_FRAME_BORDER;
        frame.loc.y += SHELL_FRAME_BORDER;
        frame.size.w -= SHELL_FRAME_BORDER * 2;
        frame.size.h -= SHELL_FRAME_BORDER * 2;
    }
    frame
}

/// Drop client-protocol fullscreen/maximize state before a shell-owned
/// configure or SUPER pointer interaction.
///
/// Denial's Flutter fullscreen is deliberately independent from XDG/EWMH
/// state. Keeping those states coupled makes a game's focus-loss request undo
/// SUPER+F and causes normal shell resize commands to be rejected.
#[cfg(feature = "flutter")]
pub(super) fn clear_client_geometry_constraints(window: &Window) -> bool {
    ManagedWindow::new(window).is_some_and(|window| window.clear_geometry_constraints())
}

/// Applies shell-owned geometry through the one managed-window adapter.
///
/// Policy callers never branch on XDG versus Xwayland. The only backend
/// distinction is the terminal protocol handshake: XDG needs a configure
/// serial, while `set_window_geometry_target` emits the X11 configure.
#[cfg(feature = "flutter")]
fn configure_shell_owned_geometry(
    state: &mut RuntimeState,
    window: &Window,
    target: Rectangle<i32, Logical>,
    authority: WindowGeometryAuthority,
) {
    clear_client_geometry_constraints(window);
    if let Some(window) = ManagedWindow::new(window) {
        window.prepare_shell_geometry(target);
    }
    state
        .wayland
        .as_mut()
        .expect("missing Wayland frontend")
        .set_window_geometry_target_with_authority(window, target, authority);
}

#[cfg(feature = "flutter")]
fn preserves_client_fullscreen_geometry(
    client_fullscreen: bool,
    current_target: Rectangle<i32, Logical>,
    requested_target: Rectangle<i32, Logical>,
) -> bool {
    client_fullscreen && current_target == requested_target
}

#[cfg(feature = "flutter")]
fn authoritative_geometry_rejects_configure(
    geometry_owned: bool,
    current_target: Rectangle<i32, Logical>,
    requested_target: Rectangle<i32, Logical>,
) -> bool {
    geometry_owned && current_target != requested_target
}

#[cfg(feature = "flutter")]
fn transfer_restore_geometry(
    mut restore: Rectangle<i32, Logical>,
    source_output: Rectangle<i32, Logical>,
    destination_output: Rectangle<i32, Logical>,
    destination_bounds: Rectangle<i32, Logical>,
) -> Rectangle<i32, Logical> {
    let translated_coordinate = |coordinate: i32, source: i32, destination: i32| {
        (i64::from(coordinate) - i64::from(source) + i64::from(destination))
            .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
    };
    restore.loc = Point::from((
        translated_coordinate(restore.loc.x, source_output.loc.x, destination_output.loc.x),
        translated_coordinate(restore.loc.y, source_output.loc.y, destination_output.loc.y),
    ));
    clamp_window_geometry(restore, destination_bounds)
}

/// Activates one managed client window through the single native focus path.
///
/// The Flutter scene owns Denial's visible z-order while Smithay and Xwayland
/// retain independent focus/stacking state. Keeping all three updates in one
/// transaction prevents a client from becoming keyboard-active underneath a
/// different visible window.
pub(super) fn activate_window(
    state: &mut RuntimeState,
    window: &Window,
    serial: smithay::utils::Serial,
) -> bool {
    #[cfg(feature = "flutter")]
    {
        let Some((window_id, minimized)) = state.wayland.as_ref().and_then(|frontend| {
            let root = frontend.window_root_surface(window)?;
            Some((
                frontend.surface_id(&root)?,
                frontend.minimized_windows.contains(&root.id()),
            ))
        }) else {
            return false;
        };
        if minimized {
            state
                .wayland
                .as_mut()
                .expect("missing Wayland frontend")
                .restore_window_workspace(window_id);
        } else if !state
            .wayland
            .as_ref()
            .expect("missing Wayland frontend")
            .window_is_on_active_workspace(window_id)
        {
            return false;
        }
    }
    let (keyboard, keyboard_focus) = {
        let frontend = state.wayland.as_ref().expect("missing Wayland frontend");
        let keyboard = frontend.seat.get_keyboard().expect("seat has no keyboard");
        let Some(keyboard_focus) = frontend.keyboard_focus_for_window(window) else {
            return false;
        };
        (keyboard, keyboard_focus)
    };

    #[cfg(feature = "flutter")]
    let window_id = state.wayland.as_ref().and_then(|frontend| {
        frontend
            .window_root_surface(window)
            .and_then(|surface| frontend.surface_id(&surface))
    });

    #[cfg(feature = "flutter")]
    let resumed;
    {
        let frontend = state.wayland.as_mut().expect("missing Wayland frontend");
        #[cfg(feature = "flutter")]
        {
            resumed = frontend
                .window_root_surface(window)
                .is_some_and(|surface| frontend.set_surface_minimized(surface.id(), false));
            if resumed && let Some(managed) = ManagedWindow::new(window) {
                managed.prepare_minimized(false);
            }
            if resumed {
                frontend.reconcile_window_layout(window);
            }
        }
        #[cfg(not(feature = "flutter"))]
        let resumed = false;

        frontend.raise_window(window, true);
        for candidate in frontend.space.elements() {
            let changed = candidate.set_activated(candidate == window);
            if let Some(managed) = ManagedWindow::new(candidate) {
                managed.prepare_activation(changed || (candidate == window && resumed));
            }
        }
    }

    request_keyboard_focus(state, &keyboard, Some(keyboard_focus), serial);
    #[cfg(feature = "flutter")]
    if let Some(window_id) = window_id {
        state
            .wayland
            .as_mut()
            .expect("missing Wayland frontend")
            .record_workspace_focus(window_id);
        state
            .pending_window_events
            .push_activation(window_id, resumed);
    }
    state.scene_sync.mark_dirty();
    true
}

/// Gives focus to the topmost remaining managed client window.
///
/// Window destruction removes the current keyboard target independently from
/// Flutter's visible stack. Re-enter the ordinary activation path so the
/// replacement receives protocol keyboard focus as well as shell activation.
#[cfg(feature = "flutter")]
pub(super) fn activate_topmost_window(state: &mut RuntimeState) -> bool {
    let next = {
        let frontend = state.wayland.as_ref().expect("missing Wayland frontend");
        frontend
            .space
            .elements()
            .rfind(|candidate| {
                ManagedWindow::new(candidate)
                    .is_some_and(|managed| !managed.facts().override_redirect)
                    && frontend.window_root_surface(candidate).is_some_and(|root| {
                        root.is_alive()
                            && !frontend.minimized_windows.contains(&root.id())
                            && frontend
                                .surface_id(&root)
                                .is_some_and(|id| frontend.window_is_on_active_workspace(id))
                    })
            })
            .cloned()
    };
    next.is_some_and(|window| activate_window(state, &window, SERIAL_COUNTER.next_serial()))
}

/// Gives focus to a usable window when a workspace has no remembered target.
///
/// Native windows retain a compositor-owned stacking order, so try them from
/// topmost to bottommost. Local Flutter windows do not currently have an
/// equivalent native stack; they remain a last-resort target for workspaces
/// that contain no activatable client window.
#[cfg(feature = "flutter")]
fn activate_workspace_fallback(
    state: &mut RuntimeState,
    monitor_id: i64,
    workspace_id: u8,
) -> bool {
    let Some(output_id) = u64::try_from(monitor_id).ok() else {
        return false;
    };
    let (client_windows, local_window_ids) = {
        let frontend = state.wayland.as_ref().expect("missing Wayland frontend");
        let belongs_to_workspace = |window_id| {
            frontend
                .workspace_location(window_id)
                .is_some_and(|location| {
                    location.output.0 == output_id && location.workspace == workspace_id
                })
        };
        let client_windows = frontend
            .space
            .elements()
            .rev()
            .filter(|candidate| {
                ManagedWindow::new(candidate)
                    .is_some_and(|managed| !managed.facts().override_redirect)
                    && frontend.window_root_surface(candidate).is_some_and(|root| {
                        root.is_alive()
                            && !frontend.minimized_windows.contains(&root.id())
                            && frontend
                                .surface_id(&root)
                                .is_some_and(&belongs_to_workspace)
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        let local_window_ids = frontend
            .local_windows
            .iter()
            .filter(|window| {
                !frontend.minimized_local_windows.contains(&window.id)
                    && belongs_to_workspace(window.id)
            })
            .map(|window| window.id)
            .collect::<Vec<_>>();
        (client_windows, local_window_ids)
    };

    for window in client_windows {
        if activate_window(state, &window, SERIAL_COUNTER.next_serial()) {
            return true;
        }
    }
    local_window_ids
        .into_iter()
        .any(|window_id| activate_local_flutter_window(state, window_id))
}

#[cfg(feature = "flutter")]
pub(in super::super) fn apply_window_commands(
    state: &mut RuntimeState,
    commands: impl IntoIterator<Item = WindowCommand>,
) -> Result<(), std::io::Error> {
    let mut had_commands = false;
    for command in commands {
        had_commands = true;
        let command = match command {
            WindowCommand::CreateLocal {
                app_id,
                title,
                geometry,
            } => {
                let created = state
                    .wayland
                    .as_mut()
                    .expect("missing Wayland frontend")
                    .create_local_flutter_window(app_id, title, geometry);
                let window_id = match created {
                    Ok(window_id) => window_id,
                    Err(error) => {
                        warn!(?error, "could not create local Flutter window");
                        continue;
                    }
                };
                activate_local_flutter_window(state, window_id);
                continue;
            }
            WindowCommand::SwitchWorkspace {
                monitor_id,
                workspace_id,
            } => {
                switch_monitor_workspace(state, monitor_id, workspace_id);
                continue;
            }
            WindowCommand::MoveToWorkspace {
                window_id,
                monitor_id,
                workspace_id,
                follow,
            } => {
                move_window_to_workspace(state, window_id, monitor_id, workspace_id, follow);
                continue;
            }
            command => command,
        };

        let window_id = command
            .window_id()
            .expect("non-create window command is missing its target");
        let is_local = state
            .wayland
            .as_ref()
            .is_some_and(|frontend| frontend.is_local_flutter_window(window_id));
        if is_local {
            match command {
                WindowCommand::Close { .. } => {
                    if state
                        .wayland
                        .as_mut()
                        .expect("missing Wayland frontend")
                        .remove_local_flutter_window(window_id)
                    {
                        state.scene_sync.mark_dirty();
                    }
                }
                WindowCommand::Focus { .. } => {
                    activate_local_flutter_window(state, window_id);
                }
                WindowCommand::Configure { geometry, .. } => {
                    if state
                        .wayland
                        .as_mut()
                        .expect("missing Wayland frontend")
                        .configure_local_flutter_window(window_id, geometry)
                    {
                        state.scene_sync.mark_dirty();
                    }
                }
                WindowCommand::CreateLocal { .. } => unreachable!(),
                WindowCommand::SwitchWorkspace { .. } | WindowCommand::MoveToWorkspace { .. } => {
                    unreachable!()
                }
            }
            continue;
        }

        let window = state
            .wayland
            .as_ref()
            .and_then(|frontend| frontend.window_for_id(window_id));
        let Some(window) = window else {
            warn!(window_id, ?command, "ignored command for stale window");
            continue;
        };
        let Some(root_surface) = state
            .wayland
            .as_ref()
            .and_then(|frontend| frontend.window_root_surface(&window))
        else {
            warn!(
                window_id,
                "ignored command for a window without a root surface"
            );
            continue;
        };

        match command {
            WindowCommand::Close { .. } => {
                close_window(&window);
            }
            WindowCommand::Focus { .. } => {
                activate_window(state, &window, SERIAL_COUNTER.next_serial());
            }
            WindowCommand::Configure {
                geometry,
                exact,
                layout_drop,
                ..
            } => {
                if layout_drop {
                    let scene_origin = state
                        .wayland
                        .as_ref()
                        .expect("missing Wayland frontend")
                        .atlas_origin;
                    let drop_location = Point::<i32, Logical>::from((
                        (geometry.x + geometry.width / 2.0 + scene_origin.x)
                            .round()
                            .clamp(f64::from(i32::MIN), f64::from(i32::MAX))
                            as i32,
                        (geometry.y + geometry.height / 2.0 + scene_origin.y)
                            .round()
                            .clamp(f64::from(i32::MIN), f64::from(i32::MAX))
                            as i32,
                    ));
                    let layout_geometry = {
                        let frontend = state.wayland.as_mut().expect("missing Wayland frontend");
                        frontend
                            .apply_layout_drop(&window, drop_location, None)
                            .then(|| frontend.window_geometry_target(&window))
                    };
                    if let Some(layout_geometry) = layout_geometry {
                        queue_transient_window_placement(
                            state,
                            &window,
                            layout_geometry,
                            WindowPlacementPhase::End,
                            WindowPlacementChange::Move,
                        );
                        state.scene_sync.mark_dirty();
                        continue;
                    }
                }
                if state
                    .wayland
                    .as_ref()
                    .is_some_and(|frontend| frontend.mobile_window_geometry(&window).is_some())
                {
                    state
                        .wayland
                        .as_mut()
                        .expect("missing Wayland frontend")
                        .configure_mobile_window(&window);
                    continue;
                }
                let layout_managed = state
                    .wayland
                    .as_ref()
                    .expect("missing Wayland frontend")
                    .window_is_layout_managed(&window);
                let geometry_owned = layout_managed
                    || state
                        .wayland
                        .as_ref()
                        .expect("missing Wayland frontend")
                        .window_geometry_authoritative(&window);
                let requested_size = Size::<i32, Logical>::from((
                    geometry.width.round() as i32,
                    geometry.height.round() as i32,
                ));
                let Some(managed) = ManagedWindow::new(&window) else {
                    continue;
                };
                let facts = managed.facts();
                if !exact && facts.client_state.resizing {
                    warn!(
                        window_id,
                        "ignored Flutter configure during an active client resize"
                    );
                    continue;
                }
                if facts.override_redirect {
                    warn!(
                        window_id,
                        "ignored Flutter configure for an unmanaged window"
                    );
                    continue;
                }
                // Fullscreen owns the output rectangle. Games commonly make
                // their current maximized resolution both the X11 minimum and
                // maximum; honoring those hints here would leave the native
                // surface maximized while Flutter stretches it fullscreen.
                let size = configured_window_size(
                    requested_size,
                    facts.minimum_size,
                    facts.maximum_size,
                    exact || geometry_owned,
                );
                let scene_origin = state
                    .wayland
                    .as_ref()
                    .expect("missing Wayland frontend")
                    .atlas_origin;
                let target_location = Point::<i32, Logical>::from((
                    (geometry.x + scene_origin.x)
                        .round()
                        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32,
                    (geometry.y + scene_origin.y)
                        .round()
                        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32,
                ));
                let target = Rectangle::new(target_location, size);
                if !exact {
                    let frontend = state.wayland.as_ref().expect("missing Wayland frontend");
                    if authoritative_geometry_rejects_configure(
                        geometry_owned,
                        frontend.window_geometry_target(&window),
                        target,
                    ) {
                        // Flutter mirrors compositor geometry for rendering and
                        // also emits interactive stacking placement. A managed
                        // layout remains the sole geometry authority, except
                        // while shell fullscreen temporarily overlays its
                        // retained tile.
                        continue;
                    }
                }
                let (preserve_client_fullscreen, transferred_shell_restore) = {
                    let frontend = state.wayland.as_mut().expect("missing Wayland frontend");
                    let client_fullscreen = facts.client_state.fullscreen;
                    let current_target = frontend.window_geometry_target(&window);
                    let preserve_client_fullscreen = !exact
                        && preserves_client_fullscreen_geometry(
                            client_fullscreen,
                            current_target,
                            target,
                        );
                    let output_transfer = frontend
                        .output_for_geometry(current_target)
                        .and_then(|source| {
                            frontend.output_for_geometry(target).map(|destination| {
                                (
                                    source.id,
                                    source.logical_geometry,
                                    destination.id,
                                    destination.logical_geometry,
                                    destination.output.clone(),
                                )
                            })
                        })
                        .filter(|(source_id, _, destination_id, _, _)| source_id != destination_id);
                    let mut transferred_shell_restore = false;
                    if let Some((_, source_geometry, _, destination_geometry, destination_output)) =
                        output_transfer
                    {
                        let destination_bounds = frontend
                            .maximize_work_area(Some(&destination_output), destination_geometry);
                        let surface_id = root_surface.id();
                        if let Some(restore) = frontend
                            .shell_maximize_restore_geometries
                            .get_mut(&surface_id)
                        {
                            *restore = transfer_restore_geometry(
                                *restore,
                                source_geometry,
                                destination_geometry,
                                destination_bounds,
                            );
                            transferred_shell_restore = true;
                        }
                        if let Some(restore) = frontend
                            .shell_fullscreen_restore_geometries
                            .get_mut(&surface_id)
                        {
                            *restore = transfer_restore_geometry(
                                *restore,
                                source_geometry,
                                destination_geometry,
                                destination_bounds,
                            );
                            transferred_shell_restore = true;
                        }
                    }
                    (preserve_client_fullscreen, transferred_shell_restore)
                };
                if !preserve_client_fullscreen {
                    // A different rectangle is a shell-authored move/resize,
                    // so the client protocol must stop constraining geometry.
                    // An identical fullscreen rectangle is only Flutter
                    // echoing the XDG/EWMH transition Rust already granted;
                    // clearing it would make browsers require a second click.
                    clear_client_geometry_constraints(&window);
                    state
                        .wayland
                        .as_mut()
                        .expect("missing Wayland frontend")
                        .restore_window_geometries
                        .remove(&root_surface.id());
                }
                managed.prepare_shell_geometry(target);
                let frontend = state.wayland.as_mut().expect("missing Wayland frontend");
                frontend.set_window_geometry_target_policy(&window, target, exact);
                if transferred_shell_restore {
                    frontend.remember_window_placement(&window);
                }
                if layout_drop {
                    queue_window_placement(
                        state,
                        &window,
                        target,
                        WindowPlacementPhase::End,
                        WindowPlacementChange::Move,
                    );
                }
                state.scene_sync.mark_dirty();
            }
            WindowCommand::CreateLocal { .. }
            | WindowCommand::SwitchWorkspace { .. }
            | WindowCommand::MoveToWorkspace { .. } => unreachable!(),
        }
    }
    // Shell commands arrive independently of client input and presentation.
    // In particular, a close sent after its preview leaves the screen must
    // reach the client even when no further client or output frame is due.
    if had_commands && let Some(frontend) = state.wayland.as_mut() {
        frontend.display_handle.flush_clients()?;
    }
    Ok(())
}

#[cfg(feature = "flutter")]
pub(super) fn switch_monitor_workspace(
    state: &mut RuntimeState,
    monitor_id: i64,
    workspace_id: u8,
) -> bool {
    let changed = state
        .wayland
        .as_mut()
        .expect("missing Wayland frontend")
        .switch_workspace(monitor_id, workspace_id);
    if !changed {
        return false;
    }

    let focused = focused_window(state);
    if let Some(window) = focused {
        let remains_visible = state.wayland.as_ref().is_some_and(|frontend| {
            frontend
                .window_root_surface(&window)
                .and_then(|root| frontend.surface_id(&root))
                .is_some_and(|window_id| frontend.window_is_on_active_workspace(window_id))
        });
        if !remains_visible {
            release_window_focus(state, &window);
        }
    }
    if let Some(local) = focused_local_window(state) {
        let remains_visible = state
            .wayland
            .as_ref()
            .is_some_and(|frontend| frontend.window_is_on_active_workspace(local));
        if !remains_visible {
            state
                .wayland
                .as_mut()
                .expect("missing Wayland frontend")
                .clear_local_flutter_focus();
        }
    }
    state.queue_workspace_action(monitor_id, workspace_id);
    let remembered = state
        .wayland
        .as_ref()
        .and_then(|frontend| frontend.remembered_workspace_focus(monitor_id, workspace_id));
    let restored_focus = remembered.is_some_and(|window_id| {
        if state
            .wayland
            .as_ref()
            .is_some_and(|frontend| frontend.is_local_flutter_window(window_id))
        {
            activate_local_flutter_window(state, window_id)
        } else if let Some(window) = state
            .wayland
            .as_ref()
            .and_then(|frontend| frontend.window_for_id(window_id))
        {
            activate_window(state, &window, SERIAL_COUNTER.next_serial())
        } else {
            false
        }
    });
    if !restored_focus {
        activate_workspace_fallback(state, monitor_id, workspace_id);
    }
    state.scene_sync.mark_dirty();
    true
}

#[cfg(feature = "flutter")]
pub(super) fn move_window_to_workspace(
    state: &mut RuntimeState,
    window_id: u64,
    monitor_id: Option<i64>,
    workspace_id: u8,
    follow: bool,
) -> bool {
    let requested_output = monitor_id
        .and_then(|monitor_id| u64::try_from(monitor_id).ok())
        .map(denial_core::topology::OutputId);
    let location = {
        let frontend = state.wayland.as_mut().expect("missing Wayland frontend");
        if frontend.minimized_local_windows.contains(&window_id) {
            frontend.set_local_flutter_window_minimized(window_id, false);
        } else if let Some(window) = frontend.window_for_id(window_id)
            && let Some(root) = frontend.window_root_surface(&window)
            && frontend.minimized_windows.contains(&root.id())
        {
            frontend.set_surface_minimized(root.id(), false);
        }
        let location = frontend.move_window_to_workspace(window_id, requested_output, workspace_id);
        if location.is_some() {
            if let Some(destination) = requested_output {
                move_window_geometry_to_output(frontend, window_id, destination);
            }
            frontend.rebuild_window_layout();
        }
        location
    };
    let Some(location) = location else {
        return false;
    };
    let monitor_id = match i64::try_from(location.output.0) {
        Ok(monitor_id) => monitor_id,
        Err(_) => return false,
    };
    if follow {
        switch_monitor_workspace(state, monitor_id, workspace_id);
        if state
            .wayland
            .as_ref()
            .is_some_and(|frontend| frontend.is_local_flutter_window(window_id))
        {
            activate_local_flutter_window(state, window_id);
        } else if let Some(window) = state
            .wayland
            .as_ref()
            .and_then(|frontend| frontend.window_for_id(window_id))
        {
            activate_window(state, &window, SERIAL_COUNTER.next_serial());
        }
    } else {
        if focused_local_window(state) == Some(window_id) {
            state
                .wayland
                .as_mut()
                .expect("missing Wayland frontend")
                .clear_local_flutter_focus();
        }
        if let Some(window) = state
            .wayland
            .as_ref()
            .and_then(|frontend| frontend.window_for_id(window_id))
        {
            release_window_focus(state, &window);
        }
    }
    state.scene_sync.mark_dirty();
    true
}

#[cfg(feature = "flutter")]
fn move_window_geometry_to_output(
    frontend: &mut super::WaylandFrontend,
    window_id: u64,
    destination: denial_core::topology::OutputId,
) {
    let Some(destination_geometry) = frontend
        .outputs
        .iter()
        .find(|output| output.id == destination)
        .map(|output| output.logical_geometry)
    else {
        return;
    };
    if frontend.is_local_flutter_window(window_id) {
        let Some(current) = frontend.local_flutter_window_geometry(window_id) else {
            return;
        };
        let current_rect = Rectangle::<i32, Logical>::new(
            Point::from((current.x.round() as i32, current.y.round() as i32)),
            Size::from((current.width.round() as i32, current.height.round() as i32)),
        );
        let Some(source_geometry) = frontend
            .output_for_geometry(current_rect)
            .map(|output| output.logical_geometry)
        else {
            return;
        };
        let target = transfer_restore_geometry(
            current_rect,
            source_geometry,
            destination_geometry,
            destination_geometry,
        );
        frontend.set_local_flutter_window_global_geometry(
            window_id,
            WindowGeometry {
                x: f64::from(target.loc.x),
                y: f64::from(target.loc.y),
                width: f64::from(target.size.w),
                height: f64::from(target.size.h),
            },
        );
        return;
    }
    let Some(window) = frontend.window_for_id(window_id) else {
        return;
    };
    let current = frontend.window_geometry_target(&window);
    let Some(source_geometry) = frontend
        .output_for_geometry(current)
        .map(|output| output.logical_geometry)
    else {
        return;
    };
    let target = transfer_restore_geometry(
        current,
        source_geometry,
        destination_geometry,
        destination_geometry,
    );
    frontend.set_window_geometry_target(&window, target);
}

#[cfg(feature = "flutter")]
pub(super) fn activate_local_flutter_window(state: &mut RuntimeState, window_id: u64) -> bool {
    let frontend = state.wayland.as_mut().expect("missing Wayland frontend");
    if frontend.minimized_local_windows.contains(&window_id) {
        frontend.set_local_flutter_window_minimized(window_id, false);
    } else if !frontend.window_is_on_active_workspace(window_id) {
        return false;
    }
    if !frontend.focus_local_flutter_window(window_id) {
        return false;
    }
    frontend.record_workspace_focus(window_id);
    let keyboard = state
        .wayland
        .as_ref()
        .expect("missing Wayland frontend")
        .seat
        .get_keyboard()
        .expect("seat has no keyboard");
    deactivate_client_windows(state.wayland.as_mut().expect("missing Wayland frontend"));
    clear_keyboard_focus(state, &keyboard, SERIAL_COUNTER.next_serial());
    state
        .pending_window_events
        .push(PendingWindowEvent::Activated(window_id));
    state.scene_sync.mark_dirty();
    true
}

#[cfg(feature = "flutter")]
fn deactivate_client_windows(frontend: &mut super::WaylandFrontend) {
    for candidate in frontend.space.elements() {
        let changed = candidate.set_activated(false);
        if let Some(managed) = ManagedWindow::new(candidate) {
            managed.prepare_activation(changed);
        }
    }
}

#[cfg(feature = "flutter")]
pub(in super::super) fn queue_local_flutter_window_placement(
    state: &mut RuntimeState,
    window_id: u64,
    phase: WindowPlacementPhase,
    change: WindowPlacementChange,
) {
    let placement = state
        .wayland
        .as_ref()
        .and_then(|frontend| frontend.local_flutter_window_placement(window_id, phase, change));
    if let Some(placement) = placement {
        state
            .pending_window_events
            .push(PendingWindowEvent::Placement(placement));
    }
}

#[cfg(feature = "flutter")]
pub(in super::super) fn queue_window_placement(
    state: &mut RuntimeState,
    window: &Window,
    geometry: Rectangle<i32, Logical>,
    phase: WindowPlacementPhase,
    change: WindowPlacementChange,
) {
    queue_window_placement_for_monitor(state, window, geometry, geometry, phase, change);
}

/// Publishes compositor-owned presentation geometry without treating it as a
/// stacking placement to restore in a future session. Layout drags use this
/// for their translated preview and their authoritative destination tile.
#[cfg(feature = "flutter")]
pub(in super::super) fn queue_transient_window_placement(
    state: &mut RuntimeState,
    window: &Window,
    geometry: Rectangle<i32, Logical>,
    phase: WindowPlacementPhase,
    change: WindowPlacementChange,
) {
    queue_window_placement_for_monitor_with_persistence(
        state, window, geometry, geometry, phase, change, false,
    );
}

/// Publishes transient geometry while retaining the layout row's physical
/// output even when a scrolling tile is mostly or completely off-screen.
#[cfg(feature = "flutter")]
pub(in super::super) fn queue_transient_window_placement_for_monitor(
    state: &mut RuntimeState,
    window: &Window,
    geometry: Rectangle<i32, Logical>,
    monitor_geometry: Rectangle<i32, Logical>,
    phase: WindowPlacementPhase,
    change: WindowPlacementChange,
) {
    queue_window_placement_for_monitor_with_persistence(
        state,
        window,
        geometry,
        monitor_geometry,
        phase,
        change,
        false,
    );
}

#[cfg(feature = "flutter")]
pub(super) fn queue_window_placement_for_monitor(
    state: &mut RuntimeState,
    window: &Window,
    geometry: Rectangle<i32, Logical>,
    monitor_geometry: Rectangle<i32, Logical>,
    phase: WindowPlacementPhase,
    change: WindowPlacementChange,
) {
    queue_window_placement_for_monitor_with_persistence(
        state,
        window,
        geometry,
        monitor_geometry,
        phase,
        change,
        true,
    );
}

#[cfg(feature = "flutter")]
pub(super) fn queue_client_window_placement_for_monitor(
    state: &mut RuntimeState,
    window: &Window,
    geometry: Rectangle<i32, Logical>,
    monitor_geometry: Rectangle<i32, Logical>,
    phase: WindowPlacementPhase,
    change: WindowPlacementChange,
) {
    queue_window_placement_for_monitor_with_persistence(
        state,
        window,
        geometry,
        monitor_geometry,
        phase,
        change,
        false,
    );
}

#[cfg(feature = "flutter")]
fn queue_window_placement_for_monitor_with_persistence(
    state: &mut RuntimeState,
    window: &Window,
    geometry: Rectangle<i32, Logical>,
    monitor_geometry: Rectangle<i32, Logical>,
    phase: WindowPlacementPhase,
    change: WindowPlacementChange,
    persist: bool,
) {
    let placement = {
        let frontend = state.wayland.as_ref().expect("missing Wayland frontend");
        let Some(placement) =
            frontend.window_placement(window, geometry, monitor_geometry, phase, change)
        else {
            return;
        };
        placement
    };
    if persist && phase == WindowPlacementPhase::End {
        state
            .wayland
            .as_mut()
            .expect("missing Wayland frontend")
            .remember_window_geometry(window, geometry);
    }
    state
        .pending_window_events
        .push(PendingWindowEvent::Placement(placement));
}

#[cfg(feature = "flutter")]
pub(super) fn queue_restored_window_state(
    state: &mut RuntimeState,
    window: &Window,
    restored: RestoredWindowPlacement,
    target: Rectangle<i32, Logical>,
) {
    queue_window_placement_for_monitor(
        state,
        window,
        restored.geometry,
        target,
        WindowPlacementPhase::End,
        WindowPlacementChange::Resize,
    );
    if restored.state.maximized {
        queue_window_action_for_window(state, window, WindowAction::Maximize);
    }
    if restored.state.fullscreen {
        queue_window_action_for_window(state, window, WindowAction::Fullscreen);
    }
}

#[cfg(feature = "flutter")]
pub(super) fn queue_window_action_for_window(
    state: &mut RuntimeState,
    window: &Window,
    action: WindowAction,
) {
    let window_id = state.wayland.as_ref().and_then(|frontend| {
        frontend
            .window_root_surface(window)
            .and_then(|surface| frontend.surface_id(&surface))
    });
    if let Some(window_id) = window_id {
        state
            .pending_window_events
            .push(PendingWindowEvent::Action(window_id, action));
    }
}

#[cfg(feature = "flutter")]
fn focused_window(state: &RuntimeState) -> Option<Window> {
    let frontend = state.wayland.as_ref()?;
    let focused = frontend.seat.get_keyboard()?.current_focus()?;
    let surface = focused.wl_surface()?;
    let root = frontend.owning_toplevel_surface(&surface)?;
    frontend.window_for_root_surface(&root)
}

#[cfg(feature = "flutter")]
pub(super) fn focus_toplevel_in_direction(
    state: &mut RuntimeState,
    direction: LayoutDirection,
) -> bool {
    let Some(focused) = focused_window(state) else {
        return false;
    };
    let target = state
        .wayland
        .as_ref()
        .expect("missing Wayland frontend")
        .layout_neighbor_window(&focused, direction);
    target.is_some_and(|target| activate_window(state, &target, SERIAL_COUNTER.next_serial()))
}

#[cfg(feature = "flutter")]
pub(super) fn swap_toplevel_in_direction(
    state: &mut RuntimeState,
    direction: LayoutDirection,
) -> bool {
    let Some(focused) = focused_window(state) else {
        return false;
    };
    let Some(target) = state
        .wayland
        .as_ref()
        .expect("missing Wayland frontend")
        .layout_neighbor_window(&focused, direction)
    else {
        return false;
    };
    let changed = state
        .wayland
        .as_mut()
        .expect("missing Wayland frontend")
        .swap_layout_windows(&focused, &target);
    if !changed {
        return false;
    }
    for window in [&focused, &target] {
        let geometry = state
            .wayland
            .as_ref()
            .expect("missing Wayland frontend")
            .window_geometry_target(window);
        queue_transient_window_placement(
            state,
            window,
            geometry,
            WindowPlacementPhase::End,
            WindowPlacementChange::Move,
        );
    }
    state.scene_sync.mark_dirty();
    true
}

/// Drop keyboard/activation focus only when `window` currently owns it.
///
/// Minimized clients remain mapped so Flutter can keep presenting their live
/// desktop widget. That makes clearing focus an explicit operation rather than
/// a side effect of unmapping the Wayland surface.
#[cfg(feature = "flutter")]
pub(super) fn release_window_focus(state: &mut RuntimeState, window: &Window) -> bool {
    if focused_window(state).as_ref() != Some(window) {
        return false;
    }

    let keyboard = state
        .wayland
        .as_ref()
        .expect("missing Wayland frontend")
        .seat
        .get_keyboard()
        .expect("seat has no keyboard");
    let changed = window.set_activated(false);
    if let Some(managed) = ManagedWindow::new(window) {
        managed.prepare_activation(changed);
    }
    clear_keyboard_focus(state, &keyboard, SERIAL_COUNTER.next_serial());
    state.scene_sync.mark_dirty();
    true
}

#[cfg(feature = "flutter")]
fn focused_local_window(state: &RuntimeState) -> Option<u64> {
    state
        .wayland
        .as_ref()
        .and_then(|frontend| frontend.focused_local_flutter_window())
}

#[cfg(feature = "flutter")]
pub(super) fn focused_workspace_window(state: &RuntimeState) -> Option<(u64, i64)> {
    let frontend = state.wayland.as_ref()?;
    let window_id = frontend.focused_local_flutter_window().or_else(|| {
        let window = focused_window(state)?;
        let root = frontend.window_root_surface(&window)?;
        frontend.surface_id(&root)
    })?;
    let location = frontend.workspace_location(window_id)?;
    Some((window_id, i64::try_from(location.output.0).ok()?))
}

#[cfg(feature = "flutter")]
fn queue_local_window_action(state: &mut RuntimeState, window_id: u64, action: WindowAction) {
    state
        .pending_window_events
        .push(PendingWindowEvent::Action(window_id, action));
    state.scene_sync.mark_dirty();
}

#[cfg(feature = "flutter")]
pub(super) fn toggle_always_on_top_focused_toplevel(state: &mut RuntimeState) -> bool {
    let local_window_id = focused_local_window(state);
    let client_window = local_window_id
        .is_none()
        .then(|| focused_window(state))
        .flatten();
    let window_id = local_window_id.or_else(|| {
        let frontend = state.wayland.as_ref()?;
        let root = frontend.window_root_surface(client_window.as_ref()?)?;
        frontend.surface_id(&root)
    });
    let Some(window_id) = window_id else {
        return false;
    };

    let pinned = {
        let frontend = state.wayland.as_mut().expect("missing Wayland frontend");
        if frontend.pinned_windows.remove(&window_id) {
            false
        } else {
            frontend.pinned_windows.insert(window_id);
            true
        }
    };
    if let Some(window) = client_window {
        let frontend = state.wayland.as_mut().expect("missing Wayland frontend");
        // Pinned windows are floating overlays in every layout. Reconcile
        // immediately so pinning collapses the vacated tile and restores the
        // saved stacking rectangle, while unpinning enrolls the current
        // floating rectangle as the next restore geometry.
        frontend.reconcile_window_layout(&window);
        if pinned {
            frontend.raise_window(&window, true);
        }
    }
    state.scene_sync.mark_dirty();
    true
}

#[cfg(feature = "flutter")]
pub(super) fn minimize_focused_toplevel(state: &mut RuntimeState) -> bool {
    if let Some(window_id) = focused_local_window(state) {
        return minimize_toplevel_by_id(state, window_id);
    }
    let Some(window) = focused_window(state) else {
        return false;
    };
    minimize_window(state, &window)
}

#[cfg(feature = "flutter")]
pub(super) fn minimize_all_toplevels(state: &mut RuntimeState) -> bool {
    let (local_window_ids, client_windows) = {
        let frontend = state.wayland.as_ref().expect("missing Wayland frontend");
        let local_window_ids = frontend
            .local_windows
            .iter()
            .filter(|window| frontend.window_is_on_active_workspace(window.id))
            .map(|window| window.id)
            .collect::<Vec<_>>();
        let client_windows = frontend
            .space
            .elements()
            .filter(|window| {
                ManagedWindow::new(window).is_some_and(|managed| !managed.facts().override_redirect)
                    && frontend.window_root_surface(window).is_some_and(|root| {
                        root.is_alive()
                            && !frontend.minimized_windows.contains(&root.id())
                            && frontend
                                .surface_id(&root)
                                .is_some_and(|id| frontend.window_is_on_active_workspace(id))
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        (local_window_ids, client_windows)
    };

    let mut minimized = false;
    for window_id in local_window_ids {
        minimized |= minimize_toplevel_by_id(state, window_id);
    }
    for window in client_windows {
        minimized |= minimize_window(state, &window);
    }
    minimized
}

#[cfg(feature = "flutter")]
pub(super) fn minimize_toplevel_by_id(state: &mut RuntimeState, window_id: u64) -> bool {
    let local = state
        .wayland
        .as_ref()
        .is_some_and(|frontend| frontend.is_local_flutter_window(window_id));
    if local {
        let frontend = state.wayland.as_mut().expect("missing Wayland frontend");
        if frontend.focused_local_flutter_window() == Some(window_id) {
            frontend.clear_local_flutter_focus();
        }
        if !frontend.set_local_flutter_window_minimized(window_id, true) {
            return false;
        }
        queue_local_window_action(state, window_id, WindowAction::Minimize);
        return true;
    }
    let Some(window) = state
        .wayland
        .as_ref()
        .and_then(|frontend| frontend.window_for_id(window_id))
    else {
        return false;
    };
    minimize_window(state, &window)
}

#[cfg(feature = "flutter")]
fn minimize_window(state: &mut RuntimeState, window: &Window) -> bool {
    apply_managed_minimize(state, window, true)
}

#[cfg(feature = "flutter")]
pub(super) fn close_focused_toplevel(state: &mut RuntimeState) -> bool {
    if let Some(window_id) = focused_local_window(state) {
        return close_toplevel_by_id(state, window_id);
    }
    let Some(window) = focused_window(state) else {
        return false;
    };
    close_window(&window)
}

#[cfg(feature = "flutter")]
pub(super) fn close_toplevel_by_id(state: &mut RuntimeState, window_id: u64) -> bool {
    let local = state
        .wayland
        .as_ref()
        .is_some_and(|frontend| frontend.is_local_flutter_window(window_id));
    if local {
        let removed = state
            .wayland
            .as_mut()
            .expect("missing Wayland frontend")
            .remove_local_flutter_window(window_id);
        if removed {
            state.scene_sync.mark_dirty();
        }
        return removed;
    }
    let Some(window) = state
        .wayland
        .as_ref()
        .and_then(|frontend| frontend.window_for_id(window_id))
    else {
        return false;
    };
    close_window(&window)
}

#[cfg(feature = "flutter")]
fn close_window(window: &Window) -> bool {
    ManagedWindow::new(window).is_some_and(|window| window.close())
}

#[cfg(feature = "flutter")]
/// Atomically applies the shell-owned SUPER+Up geometry before notifying
/// Flutter. The XDG/EWMH maximized state stays untouched, but Rust remains the
/// placement authority throughout the transition instead of waiting for a
/// later Flutter frame to return the requested coordinates.
pub(super) fn toggle_shell_maximize_focused_toplevel(state: &mut RuntimeState) -> bool {
    if let Some(window_id) = focused_local_window(state) {
        queue_local_window_action(state, window_id, WindowAction::ToggleMaximize);
        return true;
    }
    let Some(window) = focused_window(state) else {
        return false;
    };
    let client = ManagedWindow::new(&window)
        .map(|window| window.facts().client_state)
        .unwrap_or_default();

    let (target, action) = {
        let frontend = state.wayland.as_mut().expect("missing Wayland frontend");
        if client.fullscreen || frontend.window_geometry_locked(&window) {
            // SUPER+Up is a no-op while true fullscreen is active.
            return true;
        }
        let Some(root_surface) = frontend.window_root_surface(&window) else {
            return false;
        };
        let surface_id = root_surface.id();
        if let Some(restore) = frontend
            .shell_maximize_restore_geometries
            .remove(&surface_id)
        {
            frontend.restore_window_geometries.remove(&surface_id);
            (bound_geometry_size(restore), WindowAction::Restore)
        } else if client.maximized {
            let restore = frontend
                .restore_window_geometries
                .remove(&surface_id)
                .unwrap_or_else(|| frontend.window_geometry_target(&window));
            (bound_geometry_size(restore), WindowAction::Restore)
        } else {
            let restore = bound_geometry_size(frontend.window_geometry_target(&window));
            let Some(output) = frontend
                .output_for_geometry(restore)
                .map(|entry| entry.output.clone())
            else {
                return false;
            };
            let Some(output_geometry) = frontend.space.output_geometry(&output) else {
                return false;
            };
            let frame = frontend.maximize_work_area(Some(&output), output_geometry);
            let target = shell_content_geometry(frame, shell_draws_server_frame(&window));
            frontend
                .shell_maximize_restore_geometries
                .insert(surface_id, restore);
            (target, WindowAction::Maximize)
        }
    };

    let authority = if matches!(action, WindowAction::Maximize) {
        WindowGeometryAuthority::Shell
    } else {
        WindowGeometryAuthority::Pending
    };
    configure_shell_owned_geometry(state, &window, target, authority);
    if matches!(action, WindowAction::Restore) {
        state
            .wayland
            .as_mut()
            .expect("missing Wayland frontend")
            .arrange_layout_windows();
    }
    state
        .wayland
        .as_mut()
        .expect("missing Wayland frontend")
        .remember_window_placement(&window);
    // State-setting actions are deliberate here. If Flutter is still
    // reconciling a fresh window snapshot, an idempotent Restore/Maximize
    // cannot invert the shell state the compositor just applied.
    queue_window_action_for_window(state, &window, action);
    state.scene_sync.mark_dirty();
    true
}

#[cfg(feature = "flutter")]
/// Toggles a work-area-height alignment while preserving the focused window's
/// current horizontal position and width.
pub(super) fn toggle_shell_vertical_maximize_focused_toplevel(state: &mut RuntimeState) -> bool {
    if let Some(window_id) = focused_local_window(state) {
        let (target, restore) = {
            let frontend = state.wayland.as_mut().expect("missing Wayland frontend");
            let Some(current) = frontend.local_flutter_window_geometry(window_id) else {
                return false;
            };
            if let Some((y, height)) = frontend
                .local_vertical_restore_geometries
                .remove(&window_id)
            {
                (
                    WindowGeometry {
                        x: current.x,
                        y,
                        width: current.width,
                        height,
                    },
                    None,
                )
            } else {
                let current_rect = Rectangle::<i32, Logical>::new(
                    Point::from((current.x.round() as i32, current.y.round() as i32)),
                    Size::from((
                        current.width.round().max(1.0) as i32,
                        current.height.round().max(1.0) as i32,
                    )),
                );
                let Some(output) = frontend
                    .output_for_geometry(current_rect)
                    .map(|entry| entry.output.clone())
                else {
                    return false;
                };
                let Some(output_geometry) = frontend.space.output_geometry(&output) else {
                    return false;
                };
                let work_area = frontend.maximize_work_area(Some(&output), output_geometry);
                (
                    WindowGeometry {
                        x: current.x,
                        y: f64::from(work_area.loc.y),
                        width: current.width,
                        height: f64::from(work_area.size.h),
                    },
                    Some((current.y, current.height)),
                )
            }
        };
        let frontend = state.wayland.as_mut().expect("missing Wayland frontend");
        frontend.set_local_flutter_window_global_geometry(window_id, target);
        if let Some(restore) = restore {
            frontend
                .local_vertical_restore_geometries
                .insert(window_id, restore);
        }
        queue_local_flutter_window_placement(
            state,
            window_id,
            WindowPlacementPhase::End,
            WindowPlacementChange::Resize,
        );
        state.scene_sync.mark_dirty();
        return true;
    }

    let Some(window) = focused_window(state) else {
        return false;
    };
    let client_fullscreen =
        ManagedWindow::new(&window).is_some_and(|window| window.facts().client_state.fullscreen);
    let (target, restore) = {
        let frontend = state.wayland.as_mut().expect("missing Wayland frontend");
        if client_fullscreen || frontend.window_geometry_locked(&window) {
            return true;
        }
        let Some(root_surface) = frontend.window_root_surface(&window) else {
            return false;
        };
        let surface_id = root_surface.id();
        let current = bound_geometry_size(frontend.window_geometry_target(&window));
        if let Some((y, height)) = frontend
            .shell_vertical_restore_geometries
            .remove(&surface_id)
        {
            (
                Rectangle::new(
                    Point::from((current.loc.x, y)),
                    Size::from((current.size.w, height)),
                ),
                None,
            )
        } else {
            let Some(output) = frontend
                .output_for_geometry(current)
                .map(|entry| entry.output.clone())
            else {
                return false;
            };
            let Some(output_geometry) = frontend.space.output_geometry(&output) else {
                return false;
            };
            let frame = frontend.maximize_work_area(Some(&output), output_geometry);
            let content = shell_content_geometry(frame, shell_draws_server_frame(&window));
            (
                Rectangle::new(
                    Point::from((current.loc.x, content.loc.y)),
                    Size::from((current.size.w, content.size.h)),
                ),
                Some((surface_id, (current.loc.y, current.size.h))),
            )
        }
    };

    clear_client_geometry_constraints(&window);
    if let Some(window) = ManagedWindow::new(&window) {
        window.prepare_shell_geometry(target);
    }
    let frontend = state.wayland.as_mut().expect("missing Wayland frontend");
    let authority = if restore.is_some() {
        WindowGeometryAuthority::Shell
    } else {
        WindowGeometryAuthority::Pending
    };
    frontend.set_window_geometry_target_with_authority(&window, target, authority);
    if let Some((surface_id, geometry)) = restore {
        frontend
            .shell_vertical_restore_geometries
            .insert(surface_id, geometry);
    }
    queue_window_placement_for_monitor(
        state,
        &window,
        target,
        target,
        WindowPlacementPhase::End,
        WindowPlacementChange::Resize,
    );
    state.scene_sync.mark_dirty();
    true
}

#[cfg(feature = "flutter")]
pub(super) fn toggle_shell_fullscreen_focused_toplevel(state: &mut RuntimeState) -> bool {
    if let Some(window_id) = focused_local_window(state) {
        queue_local_window_action(state, window_id, WindowAction::ToggleFullscreen);
        return true;
    }
    let Some(window) = focused_window(state) else {
        return false;
    };

    // SUPER+F is compositor-owned. Rust resolves one physical output and
    // applies the complete geometry before Flutter mirrors the state; the
    // multi-output Flutter canvas is never a fullscreen target.
    let (target, action, arrange_layout) = {
        let frontend = state.wayland.as_mut().expect("missing Wayland frontend");
        let Some(root) = frontend.window_root_surface(&window) else {
            return false;
        };
        let surface_id = root.id();
        let presentation = frontend.managed_window_presentation(&window);
        let current = bound_geometry_size(frontend.window_geometry_target(&window));
        if presentation.fullscreen {
            frontend.shell_fullscreen_locks.remove(&surface_id);
            let target = frontend
                .shell_fullscreen_restore_geometries
                .remove(&surface_id)
                .or_else(|| frontend.restore_window_geometries.remove(&surface_id))
                .unwrap_or(current);
            let arrange_layout = frontend.window_is_layout_managed(&window)
                && !frontend
                    .shell_maximize_restore_geometries
                    .contains_key(&surface_id);
            (
                bound_geometry_size(target),
                WindowAction::Restore,
                arrange_layout,
            )
        } else {
            if frontend.exact_window_geometry(&window).is_some() {
                return true;
            }
            let assigned_output = frontend
                .managed_layout_space(&window)
                .map(|space| space.output);
            let output_geometry = assigned_output
                .and_then(|output| {
                    frontend
                        .outputs
                        .iter()
                        .find(|entry| entry.id == output)
                        .map(|entry| entry.logical_geometry)
                })
                .or_else(|| {
                    frontend
                        .output_for_geometry(current)
                        .map(|entry| entry.logical_geometry)
                });
            let Some(target) = output_geometry else {
                return false;
            };
            frontend
                .shell_fullscreen_restore_geometries
                .insert(surface_id.clone(), current);
            frontend.shell_fullscreen_locks.insert(surface_id);
            (target, WindowAction::Fullscreen, false)
        }
    };

    if arrange_layout {
        clear_client_geometry_constraints(&window);
        state
            .wayland
            .as_mut()
            .expect("missing Wayland frontend")
            .arrange_layout_windows();
    } else {
        let authority = if matches!(action, WindowAction::Fullscreen) {
            WindowGeometryAuthority::Shell
        } else {
            WindowGeometryAuthority::Pending
        };
        configure_shell_owned_geometry(state, &window, target, authority);
    }
    state
        .wayland
        .as_mut()
        .expect("missing Wayland frontend")
        .remember_window_placement(&window);
    queue_window_action_for_window(state, &window, action);
    state.scene_sync.mark_dirty();
    true
}

/// A protocol client request after XDG/Xwayland callback admission.
///
/// The optional output resource is meaningful only to XDG fullscreen's wire
/// acknowledgement. Output selection, geometry ownership, layout interaction,
/// and Flutter publication are otherwise identical for every managed window.
pub(super) enum ManagedClientStateRequest {
    Maximize,
    Unmaximize,
    Fullscreen(Option<wl_output::WlOutput>),
    Unfullscreen,
}

/// Applies one client state request through Denial's managed-window path.
///
/// XDG state/configure serials and X11 EWMH setters remain terminal protocol
/// handshakes. They do not own any shell policy beyond this function.
pub(super) fn apply_managed_client_state_request(
    state: &mut RuntimeState,
    window: &Window,
    request: ManagedClientStateRequest,
) -> bool {
    #[cfg(feature = "flutter")]
    if let Some(exact) = state
        .wayland
        .as_ref()
        .and_then(|frontend| frontend.exact_window_geometry(window))
    {
        configure_shell_owned_geometry(state, window, exact, WindowGeometryAuthority::Exact);
        state.scene_sync.mark_dirty();
        return true;
    }

    let entering_maximize = matches!(request, ManagedClientStateRequest::Maximize);
    if entering_maximize
        && state
            .wayland
            .as_ref()
            .expect("missing Wayland frontend")
            .window_is_layout_managed(window)
    {
        state
            .wayland
            .as_mut()
            .expect("missing Wayland frontend")
            .arrange_layout_windows();
        state.scene_sync.mark_dirty();
        return true;
    }

    let Some(root) = state
        .wayland
        .as_ref()
        .and_then(|frontend| frontend.window_root_surface(window))
    else {
        return false;
    };
    state
        .wayland
        .as_mut()
        .expect("missing Wayland frontend")
        .mark_client_geometry_state_request(&root);

    #[cfg(feature = "flutter")]
    if state
        .wayland
        .as_ref()
        .expect("missing Wayland frontend")
        .window_shell_fullscreen_locked(window)
    {
        // Shell fullscreen is the geometry authority. Reject client attempts
        // to replace it and reassert the one retained target for both protocols.
        let target = state
            .wayland
            .as_ref()
            .expect("missing Wayland frontend")
            .window_geometry_target(window);
        configure_shell_owned_geometry(state, window, target, WindowGeometryAuthority::Shell);
        state.scene_sync.mark_dirty();
        return true;
    }

    let Some(managed) = ManagedWindow::new(window) else {
        return false;
    };
    let before = managed.facts().client_state;
    let request_kind = match request {
        ManagedClientStateRequest::Maximize => ClientStateRequestKind::Maximize,
        ManagedClientStateRequest::Unmaximize => ClientStateRequestKind::Unmaximize,
        ManagedClientStateRequest::Fullscreen(_) => ClientStateRequestKind::Fullscreen,
        ManagedClientStateRequest::Unfullscreen => ClientStateRequestKind::Unfullscreen,
    };
    let entering = matches!(
        request_kind,
        ClientStateRequestKind::Maximize | ClientStateRequestKind::Fullscreen
    );
    let was_constrained = before.fullscreen || before.maximized;
    let unconstrained_after = match request_kind {
        ClientStateRequestKind::Unmaximize => !before.fullscreen,
        ClientStateRequestKind::Unfullscreen => !before.maximized,
        ClientStateRequestKind::Maximize | ClientStateRequestKind::Fullscreen => false,
    };
    let current = state
        .wayland
        .as_ref()
        .expect("missing Wayland frontend")
        .window_geometry_target(window);
    let requested_output_resource = match &request {
        ManagedClientStateRequest::Fullscreen(output) => output.as_ref(),
        _ => None,
    };
    let (target, fullscreen_output) = if entering {
        let frontend = state.wayland.as_ref().expect("missing Wayland frontend");
        let requested_output = requested_output_resource
            .and_then(Output::from_resource)
            .filter(|candidate| {
                frontend
                    .outputs
                    .iter()
                    .any(|entry| entry.output == *candidate)
            });
        let fullscreen_output = requested_output
            .as_ref()
            .and_then(|_| requested_output_resource.cloned());
        let output = requested_output
            .or_else(|| {
                frontend.managed_layout_space(window).and_then(|space| {
                    frontend
                        .outputs
                        .iter()
                        .find(|entry| entry.id == space.output)
                        .map(|entry| entry.output.clone())
                })
            })
            .or_else(|| {
                frontend
                    .output_for_geometry(current)
                    .map(|entry| entry.output.clone())
            });
        let Some(output) = output else {
            return false;
        };
        let Some(monitor) = frontend.space.output_geometry(&output) else {
            return false;
        };
        let target = if request_kind == ClientStateRequestKind::Maximize {
            frontend.maximize_work_area(Some(&output), monitor)
        } else {
            monitor
        };
        (Some(target), fullscreen_output)
    } else {
        (None, None)
    };

    let root_id = root.id();
    let restore_to_publish = if entering
        && !was_constrained
        && managed.can_store_client_restore()
        && current.size.w > 0
        && current.size.h > 0
    {
        let restore = bound_geometry_size(current);
        let frontend = state.wayland.as_mut().expect("missing Wayland frontend");
        match frontend.restore_window_geometries.entry(root_id.clone()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(restore);
                Some(restore)
            }
            std::collections::hash_map::Entry::Occupied(_) => None,
        }
    } else {
        None
    };
    let restore = if !entering && unconstrained_after {
        state
            .wayland
            .as_mut()
            .expect("missing Wayland frontend")
            .restore_window_geometries
            .remove(&root_id)
            .map(bound_geometry_size)
    } else {
        None
    };
    let target_size = target.or(restore).map(|geometry| geometry.size);
    let changed =
        managed.prepare_client_state_request(request_kind, target_size, fullscreen_output);

    if !changed {
        return false;
    }
    if let Some(target) = target {
        state
            .wayland
            .as_mut()
            .expect("missing Wayland frontend")
            .set_window_geometry_target_with_authority(
                window,
                target,
                WindowGeometryAuthority::ClientState,
            );
        #[cfg(feature = "flutter")]
        if let Some(restore) = restore_to_publish {
            queue_client_window_placement_for_monitor(
                state,
                window,
                restore,
                target,
                WindowPlacementPhase::End,
                WindowPlacementChange::Resize,
            );
        }
    } else if unconstrained_after {
        let layout_managed = state
            .wayland
            .as_ref()
            .expect("missing Wayland frontend")
            .window_is_layout_managed(window);
        if layout_managed {
            state
                .wayland
                .as_mut()
                .expect("missing Wayland frontend")
                .arrange_layout_windows();
        } else if let Some(restore) = restore {
            state
                .wayland
                .as_mut()
                .expect("missing Wayland frontend")
                .set_window_geometry_target(window, restore);
        } else {
            let frontend = state.wayland.as_mut().expect("missing Wayland frontend");
            frontend.defer_client_sized_window_placement(window);
            frontend.clear_window_geometry_intent(window);
        }
    }
    #[cfg(feature = "flutter")]
    {
        let presentation = state
            .wayland
            .as_ref()
            .expect("missing Wayland frontend")
            .managed_window_presentation(window);
        let action = if presentation.fullscreen {
            WindowAction::Fullscreen
        } else if presentation.maximized {
            WindowAction::Maximize
        } else {
            WindowAction::Restore
        };
        queue_window_action_for_window(state, window, action);
    }
    state.scene_sync.mark_dirty();
    true
}

/// Common admission gate for protocol-initiated move and resize grabs.
pub(super) fn managed_client_grab_allowed(state: &RuntimeState, window: &Window) -> bool {
    let Some(facts) = ManagedWindow::new(window).map(|window| window.facts()) else {
        return false;
    };
    if facts.override_redirect
        || facts.client_state.fullscreen
        || facts.client_state.maximized
        || state
            .wayland
            .as_ref()
            .is_some_and(|frontend| frontend.window_is_layout_managed(window))
    {
        return false;
    }
    #[cfg(feature = "flutter")]
    if state.wayland.as_ref().is_some_and(|frontend| {
        frontend.window_shell_fullscreen_locked(window)
            || frontend.exact_window_geometry(window).is_some()
    }) {
        return false;
    }
    true
}

#[cfg(feature = "flutter")]
pub(super) fn apply_managed_minimize(
    state: &mut RuntimeState,
    window: &Window,
    minimized: bool,
) -> bool {
    let Some(managed) = ManagedWindow::new(window) else {
        return false;
    };
    let Some(root) = state
        .wayland
        .as_ref()
        .and_then(|frontend| frontend.window_root_surface(window))
    else {
        return false;
    };
    managed.prepare_minimized(minimized);
    state
        .wayland
        .as_mut()
        .expect("missing Wayland frontend")
        .set_surface_minimized(root.id(), minimized);
    if minimized {
        state
            .wayland
            .as_mut()
            .expect("missing Wayland frontend")
            .remove_window_from_layout(window, false);
        release_window_focus(state, window);
        queue_window_action_for_window(state, window, WindowAction::Minimize);
    } else {
        state
            .wayland
            .as_mut()
            .expect("missing Wayland frontend")
            .reconcile_window_layout(window);
        queue_window_action_for_window(state, window, WindowAction::Restore);
    }
    state.scene_sync.mark_dirty();
    true
}

#[cfg(all(test, feature = "flutter"))]
mod tests {
    use smithay::utils::{Logical, Point, Rectangle, Size};

    use super::{authoritative_geometry_rejects_configure, configured_window_size};

    fn rect(x: i32, y: i32, width: i32, height: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((width, height)))
    }

    #[test]
    fn managed_layout_accepts_shell_fullscreen_geometry() {
        let tile = rect(10, 10, 940, 1040);
        let fullscreen = rect(0, 0, 1920, 1080);

        assert!(authoritative_geometry_rejects_configure(
            true, tile, fullscreen
        ));
        assert!(!authoritative_geometry_rejects_configure(true, tile, tile));
        assert!(!authoritative_geometry_rejects_configure(
            false, tile, fullscreen
        ));
    }

    #[test]
    fn shell_fullscreen_overrides_fixed_client_size_hints() {
        let maximized = Size::<i32, Logical>::from((2542, 1397));
        let fullscreen = Size::<i32, Logical>::from((2560, 1440));

        assert_eq!(
            configured_window_size(fullscreen, maximized, maximized, false),
            maximized,
        );
        assert_eq!(
            configured_window_size(fullscreen, maximized, maximized, true),
            fullscreen,
        );
    }

    #[test]
    fn managed_layout_overrides_fixed_client_size_hints() {
        let client_fixed = Size::<i32, Logical>::from((2280, 1397));
        let tile = Size::<i32, Logical>::from((1265, 1397));

        assert_eq!(
            configured_window_size(tile, client_fixed, client_fixed, false),
            client_fixed,
        );
        assert_eq!(
            configured_window_size(tile, client_fixed, client_fixed, true),
            tile,
        );
    }
}
