//! Generic wlr-layer-shell lifecycle and output placement.

use super::*;

impl WaylandFrontend {
    pub(super) fn layer_root_surface(&self, surface: &WlSurface) -> Option<(WlSurface, OutputId)> {
        let root = self.toplevel_candidate_surface(surface);
        self.outputs.iter().find_map(|entry| {
            let map = layer_map_for_output(&entry.output);
            map.layer_for_surface(
                &root,
                WindowSurfaceType::TOPLEVEL | WindowSurfaceType::SUBSURFACE,
            )
            .map(|layer| (layer.wl_surface().clone(), entry.id))
        })
    }

    pub(super) fn commit_layer_surface(&mut self, surface: &WlSurface) -> bool {
        let Some((root, output_id)) = self.layer_root_surface(surface) else {
            return false;
        };
        if root != *surface {
            return false;
        }
        let Some(output) = self.outputs.iter().find(|entry| entry.id == output_id) else {
            return false;
        };
        let initial_configure_sent = with_states(&root, |states| {
            states
                .data_map
                .get::<LayerSurfaceData>()
                .is_some_and(|data| {
                    data.lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .initial_configure_sent
                })
        });
        let mut map = layer_map_for_output(&output.output);
        let changed = map.arrange();
        if !initial_configure_sent
            && let Some(layer) = map.layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
        {
            layer.layer_surface().send_configure();
        }
        changed
    }

    fn set_layer_surface_scale(&self, surface: &WlSurface, output: &Output) {
        let preferred_scale =
            Self::client_preferred_scale(surface, output.current_scale().fractional_scale());
        with_states(surface, |states| {
            with_fractional_scale(states, |fractional_scale| {
                fractional_scale.set_preferred_scale(preferred_scale);
            });
        });
    }
}

impl WlrLayerShellHandler for RuntimeState {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self
            .wayland
            .as_mut()
            .expect("missing Wayland frontend")
            .layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        requested_output: Option<wl_output::WlOutput>,
        _layer: WlrLayer,
        namespace: String,
    ) {
        let frontend = self.wayland.as_mut().expect("missing Wayland frontend");
        let output = requested_output
            .as_ref()
            .and_then(Output::from_resource)
            .and_then(|requested| {
                frontend
                    .outputs
                    .iter()
                    .find(|entry| entry.output == requested)
                    .map(|entry| entry.output.clone())
            })
            .or_else(|| frontend.outputs.first().map(|entry| entry.output.clone()));
        let Some(output) = output else {
            surface.send_close();
            return;
        };

        frontend.set_layer_surface_scale(surface.wl_surface(), &output);
        let layer = DesktopLayerSurface::new(surface, namespace);
        if let Err(error) = layer_map_for_output(&output).map_layer(&layer) {
            warn!(%error, "could not map layer-shell surface");
            layer.layer_surface().send_close();
            return;
        }
        self.scene_sync.mark_dirty();
    }

    fn new_popup(&mut self, parent: WlrLayerSurface, popup: PopupSurface) {
        let frontend = self.wayland.as_mut().expect("missing Wayland frontend");
        let popup_surface = popup.wl_surface().clone();
        let _ = frontend.popups.track_popup(PopupKind::Xdg(popup));
        if let Some((_, output_id)) = frontend.layer_root_surface(parent.wl_surface())
            && let Some(output) = frontend.outputs.iter().find(|entry| entry.id == output_id)
        {
            frontend.set_layer_surface_scale(&popup_surface, &output.output);
        }
        self.scene_sync.mark_dirty();
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        let frontend = self.wayland.as_mut().expect("missing Wayland frontend");
        for output in &frontend.outputs {
            let mut map = layer_map_for_output(&output.output);
            let Some(layer) = map
                .layers()
                .find(|layer| layer.layer_surface() == &surface)
                .cloned()
            else {
                continue;
            };
            map.unmap_layer(&layer);
            break;
        }
        #[cfg(feature = "flutter")]
        frontend
            .pending_layer_frame_callback_roots
            .remove(&surface.wl_surface().id());
        frontend.remove_surface_state(surface.wl_surface(), false);
        self.scene_sync.mark_dirty();
    }
}
