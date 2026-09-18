//! Pluggable desktop window-layout algorithms.
//!
//! Layout implementations deliberately know nothing about Wayland, Smithay
//! windows, focus, or client configuration. The frontend adapter translates
//! compositor lifecycle events into stable window/output IDs and applies the
//! returned rectangles. A new layout therefore only needs to implement
//! [`WindowLayout`] and be added to [`create_window_layout`].

use std::collections::HashMap;
use std::fmt::Debug;

use denial_core::topology::OutputId;
use smithay::utils::{Logical, Point, Rectangle, Size};

/// The single ownership identity for a managed layout leaf.
///
/// Physical output and virtual workspace used to be folded into a synthetic
/// `OutputId`, while the frontend independently cached the same information.
/// Keeping the pair explicit lets every layout mutation expose and reconcile
/// its authoritative ownership without decoding geometry or duplicating state.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct LayoutSpace {
    pub(super) output: OutputId,
    pub(super) workspace: u8,
}

impl LayoutSpace {
    pub(super) const fn new(output: OutputId, workspace: u8) -> Self {
        Self { output, workspace }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum WindowLayoutKind {
    #[default]
    Stacking,
    Dwindle,
    Scrolling,
}

impl WindowLayoutKind {
    pub(super) const fn settings_name(self) -> &'static str {
        match self {
            Self::Stacking => "stacking",
            Self::Dwindle => "dwindle",
            Self::Scrolling => "scrolling",
        }
    }

    pub(super) fn from_settings_name(name: &str) -> Option<Self> {
        match name {
            "stacking" => Some(Self::Stacking),
            "dwindle" => Some(Self::Dwindle),
            "scrolling" => Some(Self::Scrolling),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LayoutInsertion<WindowId> {
    pub(super) window: WindowId,
    pub(super) space: LayoutSpace,
    /// The focused window on the destination output, when one is available.
    pub(super) anchor: Option<WindowId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LayoutPlacement<WindowId> {
    pub(super) window: WindowId,
    pub(super) geometry: Rectangle<i32, Logical>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LayoutDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum LayoutAxis {
    #[default]
    Horizontal,
    Vertical,
}

impl LayoutAxis {
    pub(super) const fn main_extent(self, geometry: Rectangle<i32, Logical>) -> i32 {
        match self {
            Self::Horizontal => geometry.size.w,
            Self::Vertical => geometry.size.h,
        }
    }

    const fn cross_extent(self, geometry: Rectangle<i32, Logical>) -> i32 {
        match self {
            Self::Horizontal => geometry.size.h,
            Self::Vertical => geometry.size.w,
        }
    }

    const fn main_size(self, size: Size<i32, Logical>) -> i32 {
        match self {
            Self::Horizontal => size.w,
            Self::Vertical => size.h,
        }
    }

    const fn cross_size(self, size: Size<i32, Logical>) -> i32 {
        match self {
            Self::Horizontal => size.h,
            Self::Vertical => size.w,
        }
    }

    const fn main_location(self, geometry: Rectangle<i32, Logical>) -> i32 {
        match self {
            Self::Horizontal => geometry.loc.x,
            Self::Vertical => geometry.loc.y,
        }
    }

    const fn cross_location(self, geometry: Rectangle<i32, Logical>) -> i32 {
        match self {
            Self::Horizontal => geometry.loc.y,
            Self::Vertical => geometry.loc.x,
        }
    }

    fn tile_geometry(
        self,
        work_area: Rectangle<i32, Logical>,
        main_location: i32,
        main_extent: i32,
    ) -> Rectangle<i32, Logical> {
        match self {
            Self::Horizontal => Rectangle::new(
                Point::from((main_location, work_area.loc.y)),
                Size::from((main_extent, work_area.size.h)),
            ),
            Self::Vertical => Rectangle::new(
                Point::from((work_area.loc.x, main_location)),
                Size::from((work_area.size.w, main_extent)),
            ),
        }
    }
}

impl LayoutDirection {
    const fn split_axis(self) -> LayoutAxis {
        match self {
            Self::Left | Self::Right => LayoutAxis::Horizontal,
            Self::Up | Self::Down => LayoutAxis::Vertical,
        }
    }

    const fn inserts_first(self) -> bool {
        matches!(self, Self::Left | Self::Up)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct LayoutResizeEdges {
    pub(super) top: bool,
    pub(super) bottom: bool,
    pub(super) left: bool,
    pub(super) right: bool,
}

impl LayoutResizeEdges {
    pub(super) const fn all() -> Self {
        Self {
            top: true,
            bottom: true,
            left: true,
            right: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct LayoutResizeRequest<WindowId> {
    pub(super) window: WindowId,
    pub(super) work_area: Rectangle<i32, Logical>,
    pub(super) gap: i32,
    /// Pointer movement since the preceding sample, in logical pixels.
    pub(super) delta_x: f64,
    pub(super) delta_y: f64,
    pub(super) edges: LayoutResizeEdges,
}

/// A deterministic geometry policy for managed desktop windows.
///
/// Implementations own only their logical arrangement. Window eligibility,
/// floating restore rectangles, protocol configures, output work areas, and
/// lifecycle reconciliation belong to the frontend adapter.
pub(super) trait WindowLayout<WindowId>: Debug
where
    WindowId: Clone + Eq + 'static,
{
    /// Clone the logical layout so interactive previews can be planned without
    /// mutating authoritative compositor state.
    fn snapshot(&self) -> Box<dyn WindowLayout<WindowId>>;

    fn kind(&self) -> WindowLayoutKind;

    /// Stacking leaves geometry under the existing free-placement policy.
    fn manages_geometry(&self) -> bool {
        true
    }

    fn insert(&mut self, insertion: LayoutInsertion<WindowId>);
    fn remove(&mut self, window: &WindowId) -> bool;
    fn contains(&self, window: &WindowId) -> bool;
    fn space_for(&self, window: &WindowId) -> Option<LayoutSpace>;
    fn clear(&mut self);

    /// Update the minimum visual size for one managed leaf. Zero-sized
    /// protocol hints are normalized by the frontend before they arrive here.
    fn update_minimum_size(&mut self, _window: &WindowId, _minimum: Size<i32, Logical>) -> bool {
        false
    }

    /// Reconcile every managed leaf after output or workspace membership
    /// changes. Layouts with additional row state may retain it here.
    fn rebuild(&mut self, insertions: Vec<LayoutInsertion<WindowId>>) {
        self.clear();
        for insertion in insertions {
            self.insert(insertion);
        }
    }

    /// Mark a managed leaf as active. Layouts with a focus-following viewport
    /// can update it here; fixed layouts may keep the default no-op.
    fn activate(&mut self, _window: &WindowId) -> bool {
        false
    }

    /// Exchange two managed leaves while preserving the layout structure.
    /// Layouts without meaningful positions may keep the default no-op.
    fn swap(&mut self, _first: &WindowId, _second: &WindowId) -> bool {
        false
    }

    /// Move one managed leaf beside another. Fixed tiling layouts may use the
    /// direction to preserve the spatial promise made by a drag preview;
    /// layouts without nested splits keep the default no-op.
    fn move_beside(
        &mut self,
        _window: &WindowId,
        _target: &WindowId,
        _direction: LayoutDirection,
    ) -> bool {
        false
    }

    /// Adjust layout-owned geometry for an interactive resize. The request is
    /// deliberately expressed without compositor or protocol types so a new
    /// layout can implement its own size policy without touching input code.
    fn resize(&mut self, _request: LayoutResizeRequest<WindowId>) -> bool {
        false
    }

    /// Resolve stateful viewport placement before an output is arranged.
    /// Fixed layouts keep the default no-op.
    fn prepare_arrange(
        &mut self,
        _space: LayoutSpace,
        _work_area: Rectangle<i32, Logical>,
        _gap: i32,
        _axis: LayoutAxis,
    ) {
    }

    /// Translate a layout-owned horizontal viewport by one gesture delta.
    /// Fixed layouts keep the default no-op.
    fn scroll_horizontally(
        &mut self,
        _space: LayoutSpace,
        _work_area: Rectangle<i32, Logical>,
        _gap: i32,
        _axis: LayoutAxis,
        _delta_x: f64,
    ) -> bool {
        false
    }

    /// Settle a translated viewport, optionally cancelling back to its
    /// original active leaf. The returned leaf should receive keyboard focus.
    fn finish_horizontal_scroll(
        &mut self,
        _space: LayoutSpace,
        _work_area: Rectangle<i32, Logical>,
        _gap: i32,
        _axis: LayoutAxis,
        _cancelled: bool,
        _projected_translation: Option<f64>,
    ) -> Option<WindowId> {
        None
    }

    /// Arrange one output. `gap` is the logical distance between siblings;
    /// outer insets are already reflected in `work_area`.
    fn arrange(
        &self,
        space: LayoutSpace,
        work_area: Rectangle<i32, Logical>,
        gap: i32,
    ) -> Vec<LayoutPlacement<WindowId>>;
}

/// Selects the visually nearest leaf in one cardinal direction. Keeping this
/// policy independent from the layout tree makes focus and keyboard swaps work
/// consistently for Dwindle, columns, master/stack, and future algorithms.
pub(super) fn directional_neighbor<WindowId>(
    focused: &WindowId,
    placements: &[LayoutPlacement<WindowId>],
    direction: LayoutDirection,
) -> Option<WindowId>
where
    WindowId: Clone + Eq,
{
    let current = placements
        .iter()
        .find(|placement| &placement.window == focused)?
        .geometry;
    let current_center = rectangle_center(current);

    placements
        .iter()
        .filter(|placement| &placement.window != focused)
        .filter_map(|placement| {
            let candidate = placement.geometry;
            let center = rectangle_center(candidate);
            let in_direction = match direction {
                LayoutDirection::Left => center.0 < current_center.0,
                LayoutDirection::Right => center.0 > current_center.0,
                LayoutDirection::Up => center.1 < current_center.1,
                LayoutDirection::Down => center.1 > current_center.1,
            };
            if !in_direction {
                return None;
            }

            let horizontal = matches!(direction, LayoutDirection::Left | LayoutDirection::Right);
            let aligned = if horizontal {
                ranges_overlap(
                    current.loc.y,
                    current.loc.y.saturating_add(current.size.h),
                    candidate.loc.y,
                    candidate.loc.y.saturating_add(candidate.size.h),
                )
            } else {
                ranges_overlap(
                    current.loc.x,
                    current.loc.x.saturating_add(current.size.w),
                    candidate.loc.x,
                    candidate.loc.x.saturating_add(candidate.size.w),
                )
            };
            let primary = if horizontal {
                (center.0 - current_center.0).abs()
            } else {
                (center.1 - current_center.1).abs()
            };
            let perpendicular = if horizontal {
                (center.1 - current_center.1).abs()
            } else {
                (center.0 - current_center.0).abs()
            };
            Some((placement, (!aligned, primary, perpendicular)))
        })
        .min_by(|(_, left), (_, right)| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.total_cmp(&right.1))
                .then_with(|| left.2.total_cmp(&right.2))
        })
        .map(|(placement, _)| placement.window.clone())
}

fn rectangle_center(rectangle: Rectangle<i32, Logical>) -> (f64, f64) {
    (
        f64::from(rectangle.loc.x) + f64::from(rectangle.size.w) / 2.0,
        f64::from(rectangle.loc.y) + f64::from(rectangle.size.h) / 2.0,
    )
}

fn ranges_overlap(first_start: i32, first_end: i32, second_start: i32, second_end: i32) -> bool {
    first_start < second_end && second_start < first_end
}

pub(super) fn create_window_layout<WindowId>(
    kind: WindowLayoutKind,
) -> Box<dyn WindowLayout<WindowId>>
where
    WindowId: Clone + Debug + Eq + 'static,
{
    match kind {
        WindowLayoutKind::Stacking => Box::<StackingLayout>::default(),
        WindowLayoutKind::Dwindle => Box::<DwindleLayout<WindowId>>::default(),
        WindowLayoutKind::Scrolling => Box::<ScrollingLayout<WindowId>>::default(),
    }
}

fn layout_minimum_size<WindowId>(
    minimum_sizes: &[(WindowId, Size<i32, Logical>)],
    window: &WindowId,
) -> Size<i32, Logical>
where
    WindowId: Eq,
{
    minimum_sizes
        .iter()
        .find_map(|(candidate, minimum)| (candidate == window).then_some(*minimum))
        .unwrap_or_else(|| Size::from((1, 1)))
}

fn update_layout_minimum_size<WindowId>(
    minimum_sizes: &mut Vec<(WindowId, Size<i32, Logical>)>,
    window: &WindowId,
    minimum: Size<i32, Logical>,
) -> bool
where
    WindowId: Clone + Eq,
{
    let minimum = Size::from((minimum.w.max(1), minimum.h.max(1)));
    if let Some((_, current)) = minimum_sizes
        .iter_mut()
        .find(|(candidate, _)| candidate == window)
    {
        if *current == minimum {
            return false;
        }
        *current = minimum;
        return true;
    }
    minimum_sizes.push((window.clone(), minimum));
    true
}

fn remove_layout_minimum_size<WindowId>(
    minimum_sizes: &mut Vec<(WindowId, Size<i32, Logical>)>,
    window: &WindowId,
) where
    WindowId: Eq,
{
    minimum_sizes.retain(|(candidate, _)| candidate != window);
}

#[derive(Clone, Debug, Default)]
struct StackingLayout;

impl<WindowId> WindowLayout<WindowId> for StackingLayout
where
    WindowId: Clone + Eq + 'static,
{
    fn snapshot(&self) -> Box<dyn WindowLayout<WindowId>> {
        Box::new(self.clone())
    }

    fn kind(&self) -> WindowLayoutKind {
        WindowLayoutKind::Stacking
    }

    fn manages_geometry(&self) -> bool {
        false
    }

    fn insert(&mut self, _insertion: LayoutInsertion<WindowId>) {}

    fn remove(&mut self, _window: &WindowId) -> bool {
        false
    }

    fn contains(&self, _window: &WindowId) -> bool {
        false
    }

    fn space_for(&self, _window: &WindowId) -> Option<LayoutSpace> {
        None
    }

    fn clear(&mut self) {}

    fn arrange(
        &self,
        _space: LayoutSpace,
        _work_area: Rectangle<i32, Logical>,
        _gap: i32,
    ) -> Vec<LayoutPlacement<WindowId>> {
        Vec::new()
    }
}

/// Hyprland-inspired dynamic binary-space-partitioning layout.
///
/// Each new window splits the focused leaf on its output (or the most recently
/// inserted leaf when focus is elsewhere). Like Hyprland's default dwindle
/// policy, split direction is derived from the current parent aspect ratio, so
/// output rotation and resizing naturally recompute ordinary splits. A
/// directional edge drop pins only its new split to the direction promised by
/// the preview. Removal collapses the now-single-child parent.
#[derive(Clone, Debug)]
struct DwindleLayout<WindowId> {
    roots: HashMap<LayoutSpace, DwindleNode<WindowId>>,
    minimum_sizes: Vec<(WindowId, Size<i32, Logical>)>,
}

impl<WindowId> Default for DwindleLayout<WindowId> {
    fn default() -> Self {
        Self {
            roots: HashMap::new(),
            minimum_sizes: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
enum DwindleNode<WindowId> {
    Window(WindowId),
    Split {
        /// Interactive edge drops pin the requested axis. Ordinary dwindle
        /// insertion remains adaptive and derives its axis from the parent.
        axis: Option<LayoutAxis>,
        ratio: f64,
        first: Box<Self>,
        second: Box<Self>,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ResizedAxes {
    horizontal: bool,
    vertical: bool,
    changed: bool,
}

impl<WindowId> DwindleNode<WindowId>
where
    WindowId: Clone + Eq,
{
    fn contains(&self, window: &WindowId) -> bool {
        match self {
            Self::Window(candidate) => candidate == window,
            Self::Split { first, second, .. } => first.contains(window) || second.contains(window),
        }
    }

    fn last_window(&self) -> &WindowId {
        match self {
            Self::Window(window) => window,
            Self::Split { second, .. } => second.last_window(),
        }
    }

    fn split_window(&mut self, anchor: &WindowId, window: WindowId) -> bool {
        match self {
            Self::Window(candidate) if candidate == anchor => {
                let previous = candidate.clone();
                *self = Self::Split {
                    axis: None,
                    ratio: 0.5,
                    first: Box::new(Self::Window(previous)),
                    second: Box::new(Self::Window(window)),
                };
                true
            }
            Self::Window(_) => false,
            Self::Split { first, second, .. } => {
                first.split_window(anchor, window.clone()) || second.split_window(anchor, window)
            }
        }
    }

    fn split_window_beside(
        &mut self,
        anchor: &WindowId,
        window: WindowId,
        direction: LayoutDirection,
    ) -> bool {
        match self {
            Self::Window(candidate) if candidate == anchor => {
                let previous = candidate.clone();
                let (first, second) = if direction.inserts_first() {
                    (Self::Window(window), Self::Window(previous))
                } else {
                    (Self::Window(previous), Self::Window(window))
                };
                *self = Self::Split {
                    axis: Some(direction.split_axis()),
                    ratio: 0.5,
                    first: Box::new(first),
                    second: Box::new(second),
                };
                true
            }
            Self::Window(_) => false,
            Self::Split { first, second, .. } => {
                first.split_window_beside(anchor, window.clone(), direction)
                    || second.split_window_beside(anchor, window, direction)
            }
        }
    }

    fn remove(self, window: &WindowId) -> (Option<Self>, bool) {
        match self {
            Self::Window(candidate) => {
                if &candidate == window {
                    (None, true)
                } else {
                    (Some(Self::Window(candidate)), false)
                }
            }
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let (first, removed) = first.remove(window);
                if removed {
                    return match first {
                        Some(first) => (
                            Some(Self::Split {
                                axis,
                                ratio,
                                first: Box::new(first),
                                second,
                            }),
                            true,
                        ),
                        None => (Some(*second), true),
                    };
                }
                let (second, removed) = second.remove(window);
                if !removed {
                    return (
                        Some(Self::Split {
                            axis,
                            ratio,
                            first: Box::new(first.expect("unchanged first dwindle child")),
                            second: Box::new(second.expect("unchanged second dwindle child")),
                        }),
                        false,
                    );
                }
                match second {
                    Some(second) => (
                        Some(Self::Split {
                            axis,
                            ratio,
                            first: Box::new(first.expect("retained first dwindle child")),
                            second: Box::new(second),
                        }),
                        true,
                    ),
                    None => (first, true),
                }
            }
        }
    }

    fn minimum_size(
        &self,
        geometry: Rectangle<i32, Logical>,
        gap: i32,
        minimum_sizes: &[(WindowId, Size<i32, Logical>)],
    ) -> Size<i32, Logical> {
        match self {
            Self::Window(window) => layout_minimum_size(minimum_sizes, window),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let axis = resolved_split_axis(*axis, geometry);
                let (first_geometry, second_geometry) =
                    split_geometry_on_axis(geometry, gap, *ratio, axis);
                let first_minimum = first.minimum_size(first_geometry, gap, minimum_sizes);
                let second_minimum = second.minimum_size(second_geometry, gap, minimum_sizes);
                match axis {
                    LayoutAxis::Horizontal => Size::from((
                        first_minimum
                            .w
                            .saturating_add(gap.max(0))
                            .saturating_add(second_minimum.w),
                        first_minimum.h.max(second_minimum.h),
                    )),
                    LayoutAxis::Vertical => Size::from((
                        first_minimum.w.max(second_minimum.w),
                        first_minimum
                            .h
                            .saturating_add(gap.max(0))
                            .saturating_add(second_minimum.h),
                    )),
                }
            }
        }
    }

    fn arrange(
        &self,
        geometry: Rectangle<i32, Logical>,
        gap: i32,
        minimum_sizes: &[(WindowId, Size<i32, Logical>)],
        placements: &mut Vec<LayoutPlacement<WindowId>>,
    ) {
        match self {
            Self::Window(window) => placements.push(LayoutPlacement {
                window: window.clone(),
                geometry,
            }),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let axis = resolved_split_axis(*axis, geometry);
                let (unconstrained_first, unconstrained_second) =
                    split_geometry_on_axis(geometry, gap, *ratio, axis);
                let first_minimum = first.minimum_size(unconstrained_first, gap, minimum_sizes);
                let second_minimum = second.minimum_size(unconstrained_second, gap, minimum_sizes);
                let (first_geometry, second_geometry) = split_geometry_on_axis_with_minimums(
                    geometry,
                    gap,
                    *ratio,
                    first_minimum,
                    second_minimum,
                    axis,
                );
                first.arrange(first_geometry, gap, minimum_sizes, placements);
                second.arrange(second_geometry, gap, minimum_sizes, placements);
            }
        }
    }

    fn resize_window(
        &mut self,
        window: &WindowId,
        geometry: Rectangle<i32, Logical>,
        gap: i32,
        edges: LayoutResizeEdges,
        delta_x: f64,
        delta_y: f64,
    ) -> ResizedAxes {
        let Self::Split {
            axis,
            ratio,
            first,
            second,
        } = self
        else {
            return ResizedAxes::default();
        };
        let window_in_first = first.contains(window);
        let window_in_second = !window_in_first && second.contains(window);
        if !window_in_first && !window_in_second {
            return ResizedAxes::default();
        }

        let axis = resolved_split_axis(*axis, geometry);
        let (first_geometry, second_geometry) = split_geometry_on_axis(geometry, gap, *ratio, axis);
        let mut resized = if window_in_first {
            first.resize_window(window, first_geometry, gap, edges, delta_x, delta_y)
        } else {
            second.resize_window(window, second_geometry, gap, edges, delta_x, delta_y)
        };

        let horizontal_split = axis == LayoutAxis::Horizontal;
        let handles_boundary = if horizontal_split {
            !resized.horizontal
                && ((window_in_first && edges.right) || (window_in_second && edges.left))
        } else {
            !resized.vertical
                && ((window_in_first && edges.bottom) || (window_in_second && edges.top))
        };
        if !handles_boundary {
            return resized;
        }

        let extent = if horizontal_split {
            geometry.size.w
        } else {
            geometry.size.h
        };
        let available = extent.saturating_sub(gap.max(0)).max(2);
        let delta = if horizontal_split { delta_x } else { delta_y };
        if delta.is_finite() && delta != 0.0 {
            let next = (*ratio + delta / f64::from(available)).clamp(0.1, 0.9);
            if (next - *ratio).abs() > f64::EPSILON {
                *ratio = next;
                resized.changed = true;
            }
        }
        if horizontal_split {
            resized.horizontal = true;
        } else {
            resized.vertical = true;
        }
        resized
    }

    fn swap_windows(&mut self, first: &WindowId, second: &WindowId) {
        match self {
            Self::Window(window) if window == first => *window = second.clone(),
            Self::Window(window) if window == second => *window = first.clone(),
            Self::Window(_) => {}
            Self::Split {
                first: first_child,
                second: second_child,
                ..
            } => {
                first_child.swap_windows(first, second);
                second_child.swap_windows(first, second);
            }
        }
    }
}

impl<WindowId> WindowLayout<WindowId> for DwindleLayout<WindowId>
where
    WindowId: Clone + Debug + Eq + 'static,
{
    fn snapshot(&self) -> Box<dyn WindowLayout<WindowId>> {
        Box::new(self.clone())
    }

    fn kind(&self) -> WindowLayoutKind {
        WindowLayoutKind::Dwindle
    }

    fn insert(&mut self, insertion: LayoutInsertion<WindowId>) {
        self.remove(&insertion.window);
        let Some(root) = self.roots.get_mut(&insertion.space) else {
            self.roots
                .insert(insertion.space, DwindleNode::Window(insertion.window));
            return;
        };
        let anchor = insertion
            .anchor
            .filter(|anchor| root.contains(anchor))
            .unwrap_or_else(|| root.last_window().clone());
        let inserted = root.split_window(&anchor, insertion.window);
        debug_assert!(inserted, "dwindle insertion anchor must exist");
    }

    fn remove(&mut self, window: &WindowId) -> bool {
        let space = self
            .roots
            .iter()
            .find_map(|(space, root)| root.contains(window).then_some(*space));
        let Some(space) = space else {
            return false;
        };
        let root = self
            .roots
            .remove(&space)
            .expect("located dwindle layout space must exist");
        let (root, removed) = root.remove(window);
        if let Some(root) = root {
            self.roots.insert(space, root);
        }
        remove_layout_minimum_size(&mut self.minimum_sizes, window);
        removed
    }

    fn contains(&self, window: &WindowId) -> bool {
        self.roots.values().any(|root| root.contains(window))
    }

    fn space_for(&self, window: &WindowId) -> Option<LayoutSpace> {
        self.roots
            .iter()
            .find_map(|(space, root)| root.contains(window).then_some(*space))
    }

    fn clear(&mut self) {
        self.roots.clear();
        self.minimum_sizes.clear();
    }

    fn update_minimum_size(&mut self, window: &WindowId, minimum: Size<i32, Logical>) -> bool {
        self.contains(window)
            && update_layout_minimum_size(&mut self.minimum_sizes, window, minimum)
    }

    fn swap(&mut self, first: &WindowId, second: &WindowId) -> bool {
        if first == second || !self.contains(first) || !self.contains(second) {
            return false;
        }
        for root in self.roots.values_mut() {
            root.swap_windows(first, second);
        }
        true
    }

    fn move_beside(
        &mut self,
        window: &WindowId,
        target: &WindowId,
        direction: LayoutDirection,
    ) -> bool {
        if window == target || !self.contains(window) || !self.contains(target) {
            return false;
        }
        let source_space = self
            .space_for(window)
            .expect("located dwindle source must have a layout space");
        let destination_space = self
            .space_for(target)
            .expect("located dwindle target must have a layout space");
        let retained_minimum = self
            .minimum_sizes
            .iter()
            .find_map(|(candidate, minimum)| (candidate == window).then_some(*minimum));
        let moved = window.clone();
        let removed = self.remove(window);
        debug_assert!(removed, "validated dwindle source must be removable");

        let inserted = self
            .roots
            .get_mut(&destination_space)
            .is_some_and(|root| root.split_window_beside(target, moved.clone(), direction));
        if !inserted {
            // Keep the operation transactional even if a future tree mutation
            // invalidates the target between validation and insertion.
            self.insert(LayoutInsertion {
                window: moved,
                space: source_space,
                anchor: None,
            });
            if let Some(minimum) = retained_minimum {
                update_layout_minimum_size(&mut self.minimum_sizes, window, minimum);
            }
            return false;
        }
        if let Some(minimum) = retained_minimum {
            update_layout_minimum_size(&mut self.minimum_sizes, window, minimum);
        }
        true
    }

    fn resize(&mut self, request: LayoutResizeRequest<WindowId>) -> bool {
        if !request.delta_x.is_finite() || !request.delta_y.is_finite() {
            return false;
        }
        let Some(root) = self
            .roots
            .values_mut()
            .find(|root| root.contains(&request.window))
        else {
            return false;
        };
        root.resize_window(
            &request.window,
            request.work_area,
            request.gap.max(0),
            request.edges,
            request.delta_x,
            request.delta_y,
        )
        .changed
    }

    fn arrange(
        &self,
        space: LayoutSpace,
        work_area: Rectangle<i32, Logical>,
        gap: i32,
    ) -> Vec<LayoutPlacement<WindowId>> {
        let Some(root) = self.roots.get(&space) else {
            return Vec::new();
        };
        let mut placements = Vec::new();
        root.arrange(work_area, gap.max(0), &self.minimum_sizes, &mut placements);
        placements
    }
}

/// A focus-following strip of tiles along the output's natural scrolling axis.
///
/// The layout borrows niri's infinite strip of columns, but gives it a
/// Denial-specific rhythm: every new column starts at three fifths of the work
/// area and may hold an ordered stack on the perpendicular axis. A lone column
/// is centered; after that the viewport moves only far enough to reveal the
/// active column and retains as many neighbors as fit. Quarter-turned outputs
/// rotate both the strip and its cross-axis stacks with the output.
#[derive(Clone, Debug)]
struct ScrollingLayout<WindowId> {
    rows: HashMap<LayoutSpace, ScrollingRow<WindowId>>,
    minimum_sizes: Vec<(WindowId, Size<i32, Logical>)>,
}

impl<WindowId> Default for ScrollingLayout<WindowId> {
    fn default() -> Self {
        Self {
            rows: HashMap::new(),
            minimum_sizes: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
struct ScrollingRow<WindowId> {
    columns: Vec<ScrollingColumn<WindowId>>,
    active: Option<WindowId>,
    axis: LayoutAxis,
    viewport_extent: i32,
    viewport_gap: i32,
    view_start: Option<f64>,
    scroll_origin: Option<f64>,
    needs_reveal: bool,
}

#[derive(Clone, Debug)]
struct ScrollingColumn<WindowId> {
    tiles: Vec<ScrollingTile<WindowId>>,
    active: WindowId,
    width_fraction: f64,
}

#[derive(Clone, Debug)]
struct ScrollingTile<WindowId> {
    window: WindowId,
    /// Relative allocation on the axis perpendicular to scrolling. Splitting
    /// one tile divides only its weight, leaving its siblings' intent intact.
    cross_weight: f64,
}

#[derive(Debug)]
struct ExtractedScrollingTile<WindowId> {
    tile: ScrollingTile<WindowId>,
    column_width_fraction: f64,
}

pub(super) const DEFAULT_SCROLLING_COLUMN_FRACTION: f64 = 3.0 / 5.0;
const MIN_SCROLLING_COLUMN_FRACTION: f64 = 1.0 / 4.0;

impl<WindowId> ScrollingColumn<WindowId>
where
    WindowId: Clone + Eq,
{
    fn single(window: WindowId, width_fraction: f64) -> Self {
        Self {
            tiles: vec![ScrollingTile {
                window: window.clone(),
                cross_weight: 1.0,
            }],
            active: window,
            width_fraction,
        }
    }

    fn position(&self, window: &WindowId) -> Option<usize> {
        self.tiles.iter().position(|tile| &tile.window == window)
    }

    fn contains(&self, window: &WindowId) -> bool {
        self.position(window).is_some()
    }

    fn normalize_cross_weights(&mut self) {
        if self.tiles.is_empty() {
            return;
        }
        let total = self
            .tiles
            .iter()
            .map(|tile| tile.cross_weight)
            .filter(|weight| weight.is_finite() && *weight > 0.0)
            .sum::<f64>();
        if total <= f64::EPSILON {
            let equal = 1.0 / self.tiles.len() as f64;
            for tile in &mut self.tiles {
                tile.cross_weight = equal;
            }
            return;
        }
        for tile in &mut self.tiles {
            tile.cross_weight = if tile.cross_weight.is_finite() && tile.cross_weight > 0.0 {
                tile.cross_weight / total
            } else {
                0.0
            };
        }
    }

    fn main_minimum(
        &self,
        minimum_sizes: &[(WindowId, Size<i32, Logical>)],
        axis: LayoutAxis,
    ) -> i32 {
        self.tiles
            .iter()
            .map(|tile| axis.main_size(layout_minimum_size(minimum_sizes, &tile.window)))
            .max()
            .unwrap_or(1)
            .max(1)
    }

    fn cross_extents(
        &self,
        work_area: Rectangle<i32, Logical>,
        gap: i32,
        minimum_sizes: &[(WindowId, Size<i32, Logical>)],
        axis: LayoutAxis,
    ) -> Vec<i32> {
        let weights = self
            .tiles
            .iter()
            .map(|tile| tile.cross_weight)
            .collect::<Vec<_>>();
        let minimums = self
            .tiles
            .iter()
            .map(|tile| axis.cross_size(layout_minimum_size(minimum_sizes, &tile.window)))
            .collect::<Vec<_>>();
        weighted_scrolling_extents(axis.cross_extent(work_area), gap, &weights, &minimums)
    }

    fn resize_cross_boundary(
        &mut self,
        tile_index: usize,
        request: &LayoutResizeRequest<WindowId>,
        axis: LayoutAxis,
        minimum_sizes: &[(WindowId, Size<i32, Logical>)],
    ) -> bool {
        let (delta, leading, trailing) = match axis {
            LayoutAxis::Horizontal => (request.delta_y, request.edges.top, request.edges.bottom),
            LayoutAxis::Vertical => (request.delta_x, request.edges.left, request.edges.right),
        };
        if self.tiles.len() < 2 || !delta.is_finite() || delta == 0.0 {
            return false;
        }
        let (first, second) = if trailing && tile_index + 1 < self.tiles.len() {
            (tile_index, tile_index + 1)
        } else if leading && tile_index > 0 {
            (tile_index - 1, tile_index)
        } else {
            return false;
        };
        let extents =
            self.cross_extents(request.work_area, request.gap.max(0), minimum_sizes, axis);
        let first_minimum = axis.cross_size(layout_minimum_size(
            minimum_sizes,
            &self.tiles[first].window,
        ));
        let second_minimum = axis.cross_size(layout_minimum_size(
            minimum_sizes,
            &self.tiles[second].window,
        ));
        let lower = f64::from(first_minimum.saturating_sub(extents[first]));
        let upper = f64::from(extents[second].saturating_sub(second_minimum));
        let delta = delta.clamp(lower, upper);
        if delta.abs() < f64::EPSILON {
            return false;
        }
        let first_extent = f64::from(extents[first]) + delta;
        let second_extent = f64::from(extents[second]) - delta;
        let combined_extent = first_extent + second_extent;
        if combined_extent <= 0.0 {
            return false;
        }
        let combined_weight = self.tiles[first].cross_weight + self.tiles[second].cross_weight;
        self.tiles[first].cross_weight = combined_weight * first_extent / combined_extent;
        self.tiles[second].cross_weight = combined_weight * second_extent / combined_extent;
        self.normalize_cross_weights();
        true
    }
}

impl<WindowId> ScrollingRow<WindowId>
where
    WindowId: Clone + Eq,
{
    fn empty() -> Self {
        Self {
            columns: Vec::new(),
            active: None,
            axis: LayoutAxis::Horizontal,
            viewport_extent: 0,
            viewport_gap: 0,
            view_start: None,
            scroll_origin: None,
            needs_reveal: true,
        }
    }

    fn position(&self, window: &WindowId) -> Option<(usize, usize)> {
        self.columns
            .iter()
            .enumerate()
            .find_map(|(column, item)| item.position(window).map(|tile| (column, tile)))
    }

    fn column_position(&self, window: &WindowId) -> Option<usize> {
        self.position(window).map(|(column, _)| column)
    }

    fn widths(
        &self,
        work_width: i32,
        minimum_sizes: &[(WindowId, Size<i32, Logical>)],
        axis: LayoutAxis,
    ) -> Vec<i64> {
        self.columns
            .iter()
            .map(|column| {
                let requested = (f64::from(work_width) * column.width_fraction)
                    .round()
                    .clamp(1.0, f64::from(work_width)) as i64;
                requested.max(i64::from(column.main_minimum(minimum_sizes, axis)))
            })
            .collect()
    }

    fn centers(widths: &[i64], gap: i32) -> Vec<f64> {
        let mut column_start = 0_i64;
        widths
            .iter()
            .map(|width| {
                let center = column_start.saturating_add(*width / 2) as f64;
                column_start = column_start
                    .saturating_add(*width)
                    .saturating_add(i64::from(gap));
                center
            })
            .collect()
    }

    fn active_index(&self) -> usize {
        self.active
            .as_ref()
            .and_then(|active| self.column_position(active))
            .unwrap_or_else(|| self.columns.len().saturating_sub(1))
    }

    fn strip_extent(widths: &[i64], gap: i32) -> f64 {
        let gaps = widths.len().saturating_sub(1) as i64 * i64::from(gap);
        widths.iter().copied().sum::<i64>().saturating_add(gaps) as f64
    }

    fn centered_view_start(widths: &[i64], gap: i32, active_index: usize, extent: i32) -> f64 {
        let active_start = widths.iter().take(active_index).fold(0_i64, |x, width| {
            x.saturating_add(*width).saturating_add(i64::from(gap))
        });
        active_start as f64 + widths[active_index] as f64 / 2.0 - f64::from(extent) / 2.0
    }

    fn constrain_view_start(view_start: f64, strip_extent: f64, viewport_extent: i32) -> f64 {
        let viewport_extent = f64::from(viewport_extent);
        if strip_extent <= viewport_extent {
            view_start.clamp(strip_extent - viewport_extent, 0.0)
        } else {
            view_start.clamp(0.0, strip_extent - viewport_extent)
        }
    }

    fn reveal_active(
        view_start: f64,
        widths: &[i64],
        gap: i32,
        active_index: usize,
        viewport_extent: i32,
    ) -> f64 {
        let active_start = widths.iter().take(active_index).fold(0_i64, |x, width| {
            x.saturating_add(*width).saturating_add(i64::from(gap))
        }) as f64;
        let active_end = active_start + widths[active_index] as f64;
        let viewport_end = view_start + f64::from(viewport_extent);
        if active_start < view_start {
            active_start
        } else if active_end > viewport_end {
            active_end - f64::from(viewport_extent)
        } else {
            view_start
        }
    }

    fn resolved_view_start(
        &self,
        work_area: Rectangle<i32, Logical>,
        gap: i32,
        axis: LayoutAxis,
        minimum_sizes: &[(WindowId, Size<i32, Logical>)],
    ) -> f64 {
        let extent = axis.main_extent(work_area).max(1);
        let widths = self.widths(extent, minimum_sizes, axis);
        let active_index = self.active_index();
        let compatible = self.axis == axis
            && self.viewport_extent == extent
            && self.viewport_gap == gap
            && self.view_start.is_some();
        let mut view_start = if compatible {
            self.view_start.unwrap_or(0.0)
        } else {
            Self::centered_view_start(&widths, gap, active_index, extent)
        };
        if self.needs_reveal && compatible {
            view_start = Self::reveal_active(view_start, &widths, gap, active_index, extent);
        }
        Self::constrain_view_start(view_start, Self::strip_extent(&widths, gap), extent)
    }

    fn prepare_arrange(
        &mut self,
        work_area: Rectangle<i32, Logical>,
        gap: i32,
        axis: LayoutAxis,
        minimum_sizes: &[(WindowId, Size<i32, Logical>)],
    ) {
        if self.columns.is_empty() {
            return;
        }
        let extent = axis.main_extent(work_area).max(1);
        self.view_start = Some(self.resolved_view_start(work_area, gap, axis, minimum_sizes));
        self.axis = axis;
        self.viewport_extent = extent;
        self.viewport_gap = gap;
        self.needs_reveal = false;
    }

    fn extract(&mut self, window: &WindowId) -> Option<ExtractedScrollingTile<WindowId>> {
        let (column_index, tile_index) = self.position(window)?;
        let active_index = self.active_index();
        let was_active = self.active.as_ref() == Some(window);
        let column_width_fraction = self.columns[column_index].width_fraction;
        let removed_weight = self.columns[column_index].tiles[tile_index].cross_weight;
        let tile = self.columns[column_index].tiles.remove(tile_index);

        if self.columns[column_index].tiles.is_empty() {
            if column_index < active_index && self.viewport_extent > 0 {
                let removed_width = (f64::from(self.viewport_extent) * column_width_fraction)
                    .round()
                    .clamp(1.0, f64::from(self.viewport_extent));
                self.view_start = self
                    .view_start
                    .map(|start| start - removed_width - f64::from(self.viewport_gap));
            }
            self.columns.remove(column_index);
            if self.columns.is_empty() {
                self.active = None;
            } else if was_active {
                let next = column_index.saturating_sub(1).min(self.columns.len() - 1);
                self.active = Some(self.columns[next].active.clone());
            }
        } else {
            let column = &mut self.columns[column_index];
            let recipient = tile_index.saturating_sub(1).min(column.tiles.len() - 1);
            column.tiles[recipient].cross_weight += removed_weight;
            if column.active == *window {
                column.active = column.tiles[tile_index.min(column.tiles.len() - 1)]
                    .window
                    .clone();
            }
            column.normalize_cross_weights();
            if was_active {
                self.active = Some(column.active.clone());
            }
        }
        self.scroll_origin = None;
        self.needs_reveal |= was_active;
        Some(ExtractedScrollingTile {
            tile,
            column_width_fraction,
        })
    }

    fn insert_beside(
        &mut self,
        extracted: ExtractedScrollingTile<WindowId>,
        target: &WindowId,
        direction: LayoutDirection,
    ) -> bool {
        let Some((column_index, tile_index)) = self.position(target) else {
            return false;
        };
        let moved = extracted.tile.window.clone();
        if direction.split_axis() == self.axis {
            let index = column_index + usize::from(!direction.inserts_first());
            self.columns.insert(
                index,
                ScrollingColumn::single(moved.clone(), extracted.column_width_fraction),
            );
        } else {
            let column = &mut self.columns[column_index];
            let target_weight = column.tiles[tile_index].cross_weight.max(f64::EPSILON);
            column.tiles[tile_index].cross_weight = target_weight / 2.0;
            let mut tile = extracted.tile;
            tile.cross_weight = target_weight / 2.0;
            let index = tile_index + usize::from(!direction.inserts_first());
            column.tiles.insert(index, tile);
            column.active = moved.clone();
            column.normalize_cross_weights();
        }
        self.active = Some(moved);
        self.scroll_origin = None;
        self.needs_reveal = true;
        true
    }

    fn scroll_horizontally(
        &mut self,
        work_area: Rectangle<i32, Logical>,
        gap: i32,
        axis: LayoutAxis,
        delta_x: f64,
        minimum_sizes: &[(WindowId, Size<i32, Logical>)],
    ) -> bool {
        if self.columns.len() < 2 || !delta_x.is_finite() || delta_x == 0.0 {
            return false;
        }
        self.prepare_arrange(work_area, gap, axis, minimum_sizes);
        let extent = axis.main_extent(work_area).max(1);
        let widths = self.widths(extent, minimum_sizes, axis);
        let strip_extent = Self::strip_extent(&widths, gap);
        let current = self.view_start.unwrap_or(0.0);
        self.scroll_origin.get_or_insert(current);
        let next = Self::constrain_view_start(current - delta_x, strip_extent, extent);
        if (next - current).abs() < f64::EPSILON {
            return false;
        }
        self.view_start = Some(next);
        true
    }

    fn finish_horizontal_scroll(
        &mut self,
        work_area: Rectangle<i32, Logical>,
        gap: i32,
        axis: LayoutAxis,
        cancelled: bool,
        projected_translation: Option<f64>,
        minimum_sizes: &[(WindowId, Size<i32, Logical>)],
    ) -> Option<WindowId> {
        if self.columns.is_empty() {
            return None;
        }
        self.prepare_arrange(work_area, gap, axis, minimum_sizes);
        let extent = axis.main_extent(work_area).max(1);
        let widths = self.widths(extent, minimum_sizes, axis);
        let centers = Self::centers(&widths, gap);
        let active_index = self.active_index();
        let selected_index = if cancelled {
            if let Some(origin) = self.scroll_origin {
                self.view_start = Some(origin);
            }
            active_index
        } else {
            let nearest_index = |view_start: f64| {
                let viewport_center = view_start + f64::from(extent) / 2.0;
                centers
                    .iter()
                    .enumerate()
                    .min_by(|(_, left), (_, right)| {
                        (**left - viewport_center)
                            .abs()
                            .total_cmp(&(**right - viewport_center).abs())
                    })
                    .map_or(active_index, |(index, _)| index)
            };
            let tracked_index = nearest_index(self.view_start.unwrap_or(0.0));
            if tracked_index != active_index {
                tracked_index
            } else if let Some(projected_translation) =
                projected_translation.filter(|translation| translation.is_finite())
            {
                let projected_view_start =
                    self.scroll_origin.unwrap_or(0.0) - projected_translation;
                match nearest_index(projected_view_start).cmp(&active_index) {
                    std::cmp::Ordering::Less => active_index.saturating_sub(1),
                    std::cmp::Ordering::Equal => active_index,
                    std::cmp::Ordering::Greater => {
                        active_index.saturating_add(1).min(centers.len() - 1)
                    }
                }
            } else {
                tracked_index
            }
        };
        let selected = self.columns[selected_index].active.clone();
        self.active = Some(selected.clone());
        self.scroll_origin = None;
        if !cancelled && selected_index != active_index {
            self.needs_reveal = true;
            self.prepare_arrange(work_area, gap, axis, minimum_sizes);
        }
        Some(selected)
    }

    fn resize(
        &mut self,
        request: &LayoutResizeRequest<WindowId>,
        minimum_sizes: &[(WindowId, Size<i32, Logical>)],
    ) -> bool {
        let Some((column_index, tile_index)) = self.position(&request.window) else {
            return false;
        };
        let (main_extent, main_delta, main_leading, main_trailing) = match self.axis {
            LayoutAxis::Horizontal => (
                request.work_area.size.w,
                request.delta_x,
                request.edges.left,
                request.edges.right,
            ),
            LayoutAxis::Vertical => (
                request.work_area.size.h,
                request.delta_y,
                request.edges.top,
                request.edges.bottom,
            ),
        };
        let mut changed = false;
        if main_delta.is_finite() && main_extent > 0 && (main_leading || main_trailing) {
            let column = &mut self.columns[column_index];
            let signed_delta = if main_leading && !main_trailing {
                -main_delta
            } else {
                main_delta
            };
            let next = (column.width_fraction + signed_delta / f64::from(main_extent))
                .clamp(MIN_SCROLLING_COLUMN_FRACTION, 1.0);
            if (next - column.width_fraction).abs() >= f64::EPSILON {
                column.width_fraction = next;
                changed = true;
            }
        }

        changed |= self.columns[column_index].resize_cross_boundary(
            tile_index,
            request,
            self.axis,
            minimum_sizes,
        );
        self.needs_reveal |= changed;
        changed
    }

    fn arrange(
        &self,
        work_area: Rectangle<i32, Logical>,
        requested_gap: i32,
        minimum_sizes: &[(WindowId, Size<i32, Logical>)],
    ) -> Vec<LayoutPlacement<WindowId>> {
        if self.columns.is_empty() {
            return Vec::new();
        }

        let axis = self.axis;
        let work_width = axis.main_extent(work_area).max(1);
        let gap = requested_gap.max(0);
        let widths = self.widths(work_width, minimum_sizes, axis);
        let view_start = self.resolved_view_start(work_area, gap, axis, minimum_sizes);
        let main_origin = axis.main_location(work_area);
        let cross_origin = axis.cross_location(work_area);
        let mut column_start = 0_i64;
        let mut placements = Vec::new();
        for (column, width) in self.columns.iter().zip(widths) {
            let location = (f64::from(main_origin) + column_start as f64 - view_start)
                .round()
                .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
            let width = width.clamp(1, i64::from(i32::MAX)) as i32;
            column_start = column_start
                .saturating_add(i64::from(width))
                .saturating_add(i64::from(gap));
            let column_geometry = axis.tile_geometry(work_area, location, width);
            let cross_extents = column.cross_extents(work_area, gap, minimum_sizes, axis);
            let mut tile_start = i64::from(cross_origin);
            for (tile, cross_extent) in column.tiles.iter().zip(cross_extents) {
                let cross_location =
                    tile_start.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
                let geometry =
                    scrolling_tile_geometry(axis, column_geometry, cross_location, cross_extent);
                placements.push(LayoutPlacement {
                    window: tile.window.clone(),
                    geometry,
                });
                tile_start = tile_start
                    .saturating_add(i64::from(cross_extent))
                    .saturating_add(i64::from(gap));
            }
        }
        placements
    }
}

impl<WindowId> WindowLayout<WindowId> for ScrollingLayout<WindowId>
where
    WindowId: Clone + Debug + Eq + 'static,
{
    fn snapshot(&self) -> Box<dyn WindowLayout<WindowId>> {
        Box::new(self.clone())
    }

    fn kind(&self) -> WindowLayoutKind {
        WindowLayoutKind::Scrolling
    }

    fn insert(&mut self, insertion: LayoutInsertion<WindowId>) {
        self.remove(&insertion.window);
        let row = self
            .rows
            .entry(insertion.space)
            .or_insert_with(ScrollingRow::empty);
        let index = insertion
            .anchor
            .as_ref()
            .and_then(|anchor| row.column_position(anchor))
            .map_or(row.columns.len(), |anchor| anchor + 1);
        row.columns.insert(
            index,
            ScrollingColumn::single(insertion.window.clone(), DEFAULT_SCROLLING_COLUMN_FRACTION),
        );
        row.active = Some(insertion.window);
        row.scroll_origin = None;
        row.needs_reveal = true;
    }

    fn remove(&mut self, window: &WindowId) -> bool {
        let space = self
            .rows
            .iter()
            .find_map(|(space, row)| row.position(window).map(|_| *space));
        let Some(space) = space else {
            return false;
        };
        let row = self.rows.get_mut(&space).expect("located scrolling row");
        let removed = row.extract(window).is_some();
        if row.columns.is_empty() {
            self.rows.remove(&space);
        }
        if removed {
            remove_layout_minimum_size(&mut self.minimum_sizes, window);
        }
        removed
    }

    fn contains(&self, window: &WindowId) -> bool {
        self.rows.values().any(|row| row.position(window).is_some())
    }

    fn space_for(&self, window: &WindowId) -> Option<LayoutSpace> {
        self.rows
            .iter()
            .find_map(|(space, row)| row.position(window).map(|_| *space))
    }

    fn clear(&mut self) {
        self.rows.clear();
        self.minimum_sizes.clear();
    }

    fn update_minimum_size(&mut self, window: &WindowId, minimum: Size<i32, Logical>) -> bool {
        self.contains(window)
            && update_layout_minimum_size(&mut self.minimum_sizes, window, minimum)
    }

    fn rebuild(&mut self, insertions: Vec<LayoutInsertion<WindowId>>) {
        let previous_rows = std::mem::take(&mut self.rows);
        let previous_widths = previous_rows
            .values()
            .flat_map(|row| &row.columns)
            .flat_map(|column| {
                column
                    .tiles
                    .iter()
                    .map(|tile| (tile.window.clone(), column.width_fraction))
            })
            .collect::<Vec<_>>();

        for (space, previous) in previous_rows {
            let previous_active_index = previous
                .active
                .as_ref()
                .and_then(|active| previous.column_position(active));
            let columns = previous
                .columns
                .into_iter()
                .filter_map(|mut column| {
                    column.tiles.retain(|tile| {
                        insertions.iter().any(|insertion| {
                            insertion.space == space && insertion.window == tile.window
                        })
                    });
                    if column.tiles.is_empty() {
                        return None;
                    }
                    if !column.contains(&column.active) {
                        column.active = column.tiles[0].window.clone();
                    }
                    column.normalize_cross_weights();
                    Some(column)
                })
                .collect::<Vec<_>>();
            if columns.is_empty() {
                continue;
            }
            let active = previous
                .active
                .filter(|active| columns.iter().any(|column| column.contains(active)))
                .or_else(|| {
                    previous_active_index
                        .map(|index| columns[index.min(columns.len() - 1)].active.clone())
                })
                .or_else(|| Some(columns[0].active.clone()));
            self.rows.insert(
                space,
                ScrollingRow {
                    columns,
                    active,
                    axis: previous.axis,
                    viewport_extent: previous.viewport_extent,
                    viewport_gap: previous.viewport_gap,
                    view_start: previous.view_start,
                    scroll_origin: None,
                    needs_reveal: true,
                },
            );
        }

        for insertion in insertions {
            if self.contains(&insertion.window) {
                continue;
            }
            let width_fraction = previous_widths
                .iter()
                .find_map(|(window, width)| (window == &insertion.window).then_some(*width))
                .unwrap_or(DEFAULT_SCROLLING_COLUMN_FRACTION);
            let row = self
                .rows
                .entry(insertion.space)
                .or_insert_with(ScrollingRow::empty);
            let index = insertion
                .anchor
                .as_ref()
                .and_then(|anchor| row.column_position(anchor))
                .map_or(row.columns.len(), |anchor| anchor + 1);
            row.columns.insert(
                index,
                ScrollingColumn::single(insertion.window.clone(), width_fraction),
            );
            row.active.get_or_insert(insertion.window);
            row.needs_reveal = true;
        }
    }

    fn activate(&mut self, window: &WindowId) -> bool {
        let Some(row) = self
            .rows
            .values_mut()
            .find(|row| row.position(window).is_some())
        else {
            return false;
        };
        let (column_index, _) = row
            .position(window)
            .expect("located scrolling tile must retain its position");
        let unchanged = row.active.as_ref() == Some(window)
            && row.columns[column_index].active == *window
            && row.scroll_origin.is_none();
        row.columns[column_index].active = window.clone();
        row.active = Some(window.clone());
        row.scroll_origin = None;
        row.needs_reveal = true;
        !unchanged
    }

    fn swap(&mut self, first: &WindowId, second: &WindowId) -> bool {
        if first == second || !self.contains(first) || !self.contains(second) {
            return false;
        }
        for row in self.rows.values_mut() {
            let mut swapped = false;
            for column in &mut row.columns {
                for tile in &mut column.tiles {
                    if &tile.window == first {
                        tile.window = second.clone();
                        swapped = true;
                    } else if &tile.window == second {
                        tile.window = first.clone();
                        swapped = true;
                    }
                }
                if &column.active == first {
                    column.active = second.clone();
                } else if &column.active == second {
                    column.active = first.clone();
                }
            }
            if row.active.as_ref() == Some(first) {
                row.active = Some(second.clone());
            } else if row.active.as_ref() == Some(second) {
                row.active = Some(first.clone());
            }
            row.needs_reveal |= swapped;
        }
        true
    }

    fn move_beside(
        &mut self,
        window: &WindowId,
        target: &WindowId,
        direction: LayoutDirection,
    ) -> bool {
        if window == target || !self.contains(window) || !self.contains(target) {
            return false;
        }
        let source_space = self
            .space_for(window)
            .expect("located scrolling source must have a layout space");
        let destination_space = self
            .space_for(target)
            .expect("located scrolling target must have a layout space");
        let extracted = self
            .rows
            .get_mut(&source_space)
            .and_then(|row| row.extract(window))
            .expect("validated scrolling source must be removable");
        if self.rows[&source_space].columns.is_empty() {
            self.rows.remove(&source_space);
        }
        let inserted = self
            .rows
            .get_mut(&destination_space)
            .is_some_and(|row| row.insert_beside(extracted, target, direction));
        debug_assert!(
            inserted,
            "validated scrolling target must remain insertable"
        );
        inserted
    }

    fn resize(&mut self, request: LayoutResizeRequest<WindowId>) -> bool {
        let Some(row) = self
            .rows
            .values_mut()
            .find(|row| row.position(&request.window).is_some())
        else {
            return false;
        };
        row.resize(&request, &self.minimum_sizes)
    }

    fn prepare_arrange(
        &mut self,
        space: LayoutSpace,
        work_area: Rectangle<i32, Logical>,
        gap: i32,
        axis: LayoutAxis,
    ) {
        let (rows, minimum_sizes) = (&mut self.rows, &self.minimum_sizes);
        if let Some(row) = rows.get_mut(&space) {
            row.prepare_arrange(work_area, gap.max(0), axis, minimum_sizes);
        }
    }

    fn scroll_horizontally(
        &mut self,
        space: LayoutSpace,
        work_area: Rectangle<i32, Logical>,
        gap: i32,
        axis: LayoutAxis,
        delta_x: f64,
    ) -> bool {
        let (rows, minimum_sizes) = (&mut self.rows, &self.minimum_sizes);
        rows.get_mut(&space).is_some_and(|row| {
            row.scroll_horizontally(work_area, gap.max(0), axis, delta_x, minimum_sizes)
        })
    }

    fn finish_horizontal_scroll(
        &mut self,
        space: LayoutSpace,
        work_area: Rectangle<i32, Logical>,
        gap: i32,
        axis: LayoutAxis,
        cancelled: bool,
        projected_translation: Option<f64>,
    ) -> Option<WindowId> {
        let (rows, minimum_sizes) = (&mut self.rows, &self.minimum_sizes);
        rows.get_mut(&space)?.finish_horizontal_scroll(
            work_area,
            gap.max(0),
            axis,
            cancelled,
            projected_translation,
            minimum_sizes,
        )
    }

    fn arrange(
        &self,
        space: LayoutSpace,
        work_area: Rectangle<i32, Logical>,
        gap: i32,
    ) -> Vec<LayoutPlacement<WindowId>> {
        self.rows.get(&space).map_or_else(Vec::new, |row| {
            row.arrange(work_area, gap, &self.minimum_sizes)
        })
    }
}

fn weighted_scrolling_extents(
    requested_extent: i32,
    requested_gap: i32,
    weights: &[f64],
    minimums: &[i32],
) -> Vec<i32> {
    debug_assert_eq!(weights.len(), minimums.len());
    if weights.is_empty() {
        return Vec::new();
    }
    let gap_total =
        i64::from(requested_gap.max(0)).saturating_mul(weights.len().saturating_sub(1) as i64);
    let available = i64::from(requested_extent.max(1))
        .saturating_sub(gap_total)
        .max(weights.len() as i64);
    let minimums = minimums
        .iter()
        .map(|minimum| i64::from((*minimum).max(1)))
        .collect::<Vec<_>>();
    if minimums.iter().copied().sum::<i64>() > available {
        return minimums
            .into_iter()
            .map(|extent| extent.min(i64::from(i32::MAX)) as i32)
            .collect();
    }

    let weights = weights
        .iter()
        .map(|weight| {
            if weight.is_finite() && *weight > 0.0 {
                *weight
            } else {
                1.0
            }
        })
        .collect::<Vec<_>>();
    let mut extents = vec![None; weights.len()];
    let mut remaining_extent = available;
    let mut remaining_weight = weights.iter().sum::<f64>();
    loop {
        let constrained = extents
            .iter()
            .enumerate()
            .filter(|(_, extent)| extent.is_none())
            .find_map(|(index, _)| {
                let requested = remaining_extent as f64 * weights[index] / remaining_weight;
                (requested < minimums[index] as f64).then_some(index)
            });
        let Some(index) = constrained else {
            break;
        };
        extents[index] = Some(minimums[index]);
        remaining_extent = remaining_extent.saturating_sub(minimums[index]);
        remaining_weight -= weights[index];
    }

    let remaining = extents
        .iter()
        .enumerate()
        .filter_map(|(index, extent)| extent.is_none().then_some(index))
        .collect::<Vec<_>>();
    let mut distributed = 0_i64;
    for (position, index) in remaining.iter().copied().enumerate() {
        let extent = if position + 1 == remaining.len() {
            remaining_extent.saturating_sub(distributed)
        } else {
            let share = (remaining_extent as f64 * weights[index] / remaining_weight).round();
            let reserved_minimum = remaining[position + 1..]
                .iter()
                .map(|index| minimums[*index])
                .sum::<i64>();
            (share as i64).clamp(
                minimums[index],
                remaining_extent
                    .saturating_sub(distributed)
                    .saturating_sub(reserved_minimum),
            )
        };
        extents[index] = Some(extent);
        distributed = distributed.saturating_add(extent);
    }
    extents
        .into_iter()
        .map(|extent| extent.unwrap_or(1).clamp(1, i64::from(i32::MAX)) as i32)
        .collect()
}

fn scrolling_tile_geometry(
    axis: LayoutAxis,
    column: Rectangle<i32, Logical>,
    cross_location: i32,
    cross_extent: i32,
) -> Rectangle<i32, Logical> {
    match axis {
        LayoutAxis::Horizontal => Rectangle::new(
            Point::from((column.loc.x, cross_location)),
            Size::from((column.size.w, cross_extent)),
        ),
        LayoutAxis::Vertical => Rectangle::new(
            Point::from((cross_location, column.loc.y)),
            Size::from((cross_extent, column.size.h)),
        ),
    }
}

fn resolved_split_axis(
    requested: Option<LayoutAxis>,
    geometry: Rectangle<i32, Logical>,
) -> LayoutAxis {
    requested.unwrap_or_else(|| {
        if geometry.size.w >= geometry.size.h {
            LayoutAxis::Horizontal
        } else {
            LayoutAxis::Vertical
        }
    })
}

fn split_geometry_on_axis(
    geometry: Rectangle<i32, Logical>,
    requested_gap: i32,
    requested_ratio: f64,
    axis: LayoutAxis,
) -> (Rectangle<i32, Logical>, Rectangle<i32, Logical>) {
    split_geometry_on_axis_with_minimums(
        geometry,
        requested_gap,
        requested_ratio,
        Size::from((1, 1)),
        Size::from((1, 1)),
        axis,
    )
}

fn split_geometry_on_axis_with_minimums(
    geometry: Rectangle<i32, Logical>,
    requested_gap: i32,
    requested_ratio: f64,
    first_minimum: Size<i32, Logical>,
    second_minimum: Size<i32, Logical>,
    axis: LayoutAxis,
) -> (Rectangle<i32, Logical>, Rectangle<i32, Logical>) {
    let horizontal = axis == LayoutAxis::Horizontal;
    let extent = if horizontal {
        geometry.size.w
    } else {
        geometry.size.h
    }
    .max(2);
    let gap = requested_gap.clamp(0, extent.saturating_sub(2));
    let available = extent.saturating_sub(gap);
    let ratio = if requested_ratio.is_finite() {
        requested_ratio.clamp(0.1, 0.9)
    } else {
        0.5
    };
    let requested_first = (f64::from(available) * ratio).round() as i32;
    let first_minimum_extent = if horizontal {
        first_minimum.w
    } else {
        first_minimum.h
    }
    .max(1);
    let second_minimum_extent = if horizontal {
        second_minimum.w
    } else {
        second_minimum.h
    }
    .max(1);
    let minimum_sum = first_minimum_extent.saturating_add(second_minimum_extent);
    let (first_extent, second_extent) = if minimum_sum <= available {
        let first_extent = requested_first.clamp(
            first_minimum_extent,
            available.saturating_sub(second_minimum_extent),
        );
        (first_extent, available.saturating_sub(first_extent))
    } else {
        // When the clients' combined minima cannot fit, preserve both
        // contracts and let the layout overflow the work area. Undersizing a
        // client is not a viable fallback: some toolkits will reject the
        // configure and can otherwise enter a configure/commit feedback loop.
        (first_minimum_extent, second_minimum_extent)
    };

    if horizontal {
        (
            Rectangle::new(
                geometry.loc,
                Size::from((first_extent, geometry.size.h.max(first_minimum.h))),
            ),
            Rectangle::new(
                Point::from((
                    geometry
                        .loc
                        .x
                        .saturating_add(first_extent)
                        .saturating_add(gap),
                    geometry.loc.y,
                )),
                Size::from((second_extent, geometry.size.h.max(second_minimum.h))),
            ),
        )
    } else {
        (
            Rectangle::new(
                geometry.loc,
                Size::from((geometry.size.w.max(first_minimum.w), first_extent)),
            ),
            Rectangle::new(
                Point::from((
                    geometry.loc.x,
                    geometry
                        .loc
                        .y
                        .saturating_add(first_extent)
                        .saturating_add(gap),
                )),
                Size::from((geometry.size.w.max(second_minimum.w), second_extent)),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTPUT: LayoutSpace = LayoutSpace::new(OutputId(1), 1);
    const SECOND_OUTPUT: LayoutSpace = LayoutSpace::new(OutputId(2), 1);
    const SECOND_WORKSPACE: LayoutSpace = LayoutSpace::new(OutputId(1), 2);

    fn rect(x: i32, y: i32, width: i32, height: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((width, height)))
    }

    fn prepare_scrolling(
        layout: &mut ScrollingLayout<u64>,
        work_area: Rectangle<i32, Logical>,
        gap: i32,
        axis: LayoutAxis,
    ) {
        layout.prepare_arrange(OUTPUT, work_area, gap, axis);
    }

    #[test]
    fn dwindle_splits_the_focused_leaf_and_uses_parent_aspect_ratio() {
        let mut layout = DwindleLayout::<u64>::default();
        layout.insert(LayoutInsertion {
            window: 1,
            space: OUTPUT,
            anchor: None,
        });
        layout.insert(LayoutInsertion {
            window: 2,
            space: OUTPUT,
            anchor: Some(1),
        });
        layout.insert(LayoutInsertion {
            window: 3,
            space: OUTPUT,
            anchor: Some(2),
        });

        assert_eq!(
            layout.arrange(OUTPUT, rect(10, 20, 1000, 600), 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(10, 20, 495, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(515, 20, 495, 295),
                },
                LayoutPlacement {
                    window: 3,
                    geometry: rect(515, 325, 495, 295),
                },
            ]
        );
    }

    #[test]
    fn dwindle_respects_minimum_widths_when_they_fit() {
        let mut layout = DwindleLayout::<u64>::default();
        layout.insert(LayoutInsertion {
            window: 1,
            space: OUTPUT,
            anchor: None,
        });
        layout.insert(LayoutInsertion {
            window: 2,
            space: OUTPUT,
            anchor: Some(1),
        });
        assert!(layout.update_minimum_size(&1, Size::from((700, 1))));
        assert!(layout.update_minimum_size(&2, Size::from((200, 1))));

        assert_eq!(
            layout.arrange(OUTPUT, rect(0, 0, 1000, 600), 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(0, 0, 700, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(710, 0, 290, 600),
                },
            ]
        );
    }

    #[test]
    fn dwindle_overflows_instead_of_undersizing_impossible_minima() {
        let mut layout = DwindleLayout::<u64>::default();
        layout.insert(LayoutInsertion {
            window: 1,
            space: OUTPUT,
            anchor: None,
        });
        layout.insert(LayoutInsertion {
            window: 2,
            space: OUTPUT,
            anchor: Some(1),
        });
        assert!(layout.update_minimum_size(&1, Size::from((700, 1))));
        assert!(layout.update_minimum_size(&2, Size::from((600, 1))));

        assert_eq!(
            layout.arrange(OUTPUT, rect(0, 0, 1000, 600), 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(0, 0, 700, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(710, 0, 600, 600),
                },
            ]
        );
    }

    #[test]
    fn removing_a_leaf_collapses_its_parent_without_disturbing_other_outputs() {
        let mut layout = DwindleLayout::<u64>::default();
        for window in 1..=3 {
            layout.insert(LayoutInsertion {
                window,
                space: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
        }
        layout.insert(LayoutInsertion {
            window: 4,
            space: SECOND_OUTPUT,
            anchor: None,
        });

        assert!(layout.remove(&2));
        assert!(!layout.contains(&2));
        assert_eq!(
            layout.arrange(OUTPUT, rect(0, 0, 800, 600), 0),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(0, 0, 400, 600),
                },
                LayoutPlacement {
                    window: 3,
                    geometry: rect(400, 0, 400, 600),
                },
            ]
        );
        assert_eq!(
            layout.arrange(SECOND_OUTPUT, rect(800, 0, 800, 600), 0),
            vec![LayoutPlacement {
                window: 4,
                geometry: rect(800, 0, 800, 600),
            }]
        );
    }

    #[test]
    fn stacking_explicitly_leaves_geometry_unmanaged() {
        let mut layout = create_window_layout::<u64>(WindowLayoutKind::Stacking);
        layout.insert(LayoutInsertion {
            window: 1,
            space: OUTPUT,
            anchor: None,
        });
        assert!(!layout.manages_geometry());
        assert!(layout.arrange(OUTPUT, rect(0, 0, 800, 600), 10).is_empty());
    }

    #[test]
    fn dwindle_swaps_leaves_without_rebuilding_the_tree() {
        let mut layout = DwindleLayout::<u64>::default();
        for window in 1..=3 {
            layout.insert(LayoutInsertion {
                window,
                space: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
        }

        assert!(layout.swap(&1, &3));
        assert_eq!(
            layout.arrange(OUTPUT, rect(0, 0, 1000, 600), 0),
            vec![
                LayoutPlacement {
                    window: 3,
                    geometry: rect(0, 0, 500, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(500, 0, 500, 300),
                },
                LayoutPlacement {
                    window: 1,
                    geometry: rect(500, 300, 500, 300),
                },
            ]
        );
    }

    #[test]
    fn dwindle_edge_drop_places_the_moved_leaf_on_the_promised_side() {
        for direction in [
            LayoutDirection::Left,
            LayoutDirection::Right,
            LayoutDirection::Up,
            LayoutDirection::Down,
        ] {
            let mut layout = DwindleLayout::<u64>::default();
            for window in 1..=3 {
                layout.insert(LayoutInsertion {
                    window,
                    space: OUTPUT,
                    anchor: (window > 1).then_some(window - 1),
                });
            }

            assert!(layout.move_beside(&1, &3, direction));
            let placements = layout.arrange(OUTPUT, rect(0, 0, 1000, 600), 10);
            let moved = placements
                .iter()
                .find(|placement| placement.window == 1)
                .expect("moved leaf remains arranged")
                .geometry;
            let target = placements
                .iter()
                .find(|placement| placement.window == 3)
                .expect("target leaf remains arranged")
                .geometry;
            match direction {
                LayoutDirection::Left => {
                    assert!(moved.loc.x.saturating_add(moved.size.w) <= target.loc.x)
                }
                LayoutDirection::Right => {
                    assert!(target.loc.x.saturating_add(target.size.w) <= moved.loc.x)
                }
                LayoutDirection::Up => {
                    assert!(moved.loc.y.saturating_add(moved.size.h) <= target.loc.y)
                }
                LayoutDirection::Down => {
                    assert!(target.loc.y.saturating_add(target.size.h) <= moved.loc.y)
                }
            }
        }
    }

    #[test]
    fn dwindle_edge_drop_preserves_the_moved_leaf_minimum_size() {
        let mut layout = DwindleLayout::<u64>::default();
        layout.insert(LayoutInsertion {
            window: 1,
            space: OUTPUT,
            anchor: None,
        });
        layout.insert(LayoutInsertion {
            window: 2,
            space: OUTPUT,
            anchor: Some(1),
        });
        assert!(layout.update_minimum_size(&1, Size::from((320, 240))));

        assert!(layout.move_beside(&1, &2, LayoutDirection::Down));
        assert_eq!(
            layout_minimum_size(&layout.minimum_sizes, &1),
            Size::from((320, 240)),
        );
    }

    #[test]
    fn dwindle_cross_space_swap_exchanges_authoritative_ownership() {
        let mut layout = DwindleLayout::<u64>::default();
        layout.insert(LayoutInsertion {
            window: 1,
            space: OUTPUT,
            anchor: None,
        });
        layout.insert(LayoutInsertion {
            window: 2,
            space: SECOND_WORKSPACE,
            anchor: None,
        });

        assert!(layout.swap(&1, &2));
        assert_eq!(layout.space_for(&1), Some(SECOND_WORKSPACE));
        assert_eq!(layout.space_for(&2), Some(OUTPUT));
        assert_eq!(layout.arrange(OUTPUT, rect(0, 0, 800, 600), 0)[0].window, 2);
        assert_eq!(
            layout.arrange(SECOND_WORKSPACE, rect(0, 0, 800, 600), 0)[0].window,
            1
        );
    }

    #[test]
    fn directional_navigation_prefers_aligned_tiles() {
        let placements = vec![
            LayoutPlacement {
                window: 1,
                geometry: rect(0, 0, 500, 600),
            },
            LayoutPlacement {
                window: 2,
                geometry: rect(500, 0, 500, 300),
            },
            LayoutPlacement {
                window: 3,
                geometry: rect(500, 300, 500, 300),
            },
        ];

        assert_eq!(
            directional_neighbor(&1, &placements, LayoutDirection::Right),
            Some(2)
        );
        assert_eq!(
            directional_neighbor(&2, &placements, LayoutDirection::Down),
            Some(3)
        );
        assert_eq!(
            directional_neighbor(&3, &placements, LayoutDirection::Up),
            Some(2)
        );
        assert_eq!(
            directional_neighbor(&2, &placements, LayoutDirection::Left),
            Some(1)
        );
    }

    #[test]
    fn dwindle_resizes_the_nearest_matching_split() {
        let mut layout = DwindleLayout::<u64>::default();
        for window in 1..=3 {
            layout.insert(LayoutInsertion {
                window,
                space: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
        }

        assert!(layout.resize(LayoutResizeRequest {
            window: 2,
            work_area: rect(0, 0, 1000, 600),
            gap: 0,
            delta_x: 0.0,
            delta_y: 60.0,
            edges: LayoutResizeEdges {
                bottom: true,
                ..LayoutResizeEdges::default()
            },
        }));
        assert_eq!(
            layout.arrange(OUTPUT, rect(0, 0, 1000, 600), 0),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(0, 0, 500, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(500, 0, 500, 360),
                },
                LayoutPlacement {
                    window: 3,
                    geometry: rect(500, 360, 500, 240),
                },
            ]
        );
    }

    #[test]
    fn scrolling_column_width_respects_the_window_minimum() {
        let mut layout = ScrollingLayout::<u64>::default();
        let work_area = rect(0, 0, 1000, 600);
        layout.insert(LayoutInsertion {
            window: 1,
            space: OUTPUT,
            anchor: None,
        });
        assert!(layout.update_minimum_size(&1, Size::from((800, 700))));
        prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);

        let placement = layout.arrange(OUTPUT, work_area, 10).remove(0);
        assert_eq!(placement.geometry.size, Size::from((800, 700)));
    }

    #[test]
    fn scrolling_cross_axis_drop_splits_only_the_target_tile() {
        let mut layout = ScrollingLayout::<u64>::default();
        let work_area = rect(0, 0, 1000, 600);
        for window in 1..=3 {
            layout.insert(LayoutInsertion {
                window,
                space: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
            prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        }

        assert!(layout.move_beside(&1, &3, LayoutDirection::Up));
        prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        let row = &layout.rows[&OUTPUT];
        assert_eq!(row.columns.len(), 2);
        assert_eq!(
            row.columns[1]
                .tiles
                .iter()
                .map(|tile| tile.window)
                .collect::<Vec<_>>(),
            vec![1, 3],
        );

        let placements = layout.arrange(OUTPUT, work_area, 10);
        let moved = placements
            .iter()
            .find(|placement| placement.window == 1)
            .expect("moved tile remains arranged")
            .geometry;
        let target = placements
            .iter()
            .find(|placement| placement.window == 3)
            .expect("target tile remains arranged")
            .geometry;
        assert_eq!(moved.size.h, 295);
        assert_eq!(target.size.h, 295);
        assert_eq!(moved.loc.x, target.loc.x);
        assert_eq!(moved.size.w, target.size.w);
        assert_eq!(moved.loc.y.saturating_add(moved.size.h + 10), target.loc.y);
    }

    #[test]
    fn scrolling_repeated_cross_axis_drops_preserve_sibling_weight() {
        let mut layout = ScrollingLayout::<u64>::default();
        let work_area = rect(0, 0, 1000, 600);
        for window in 1..=3 {
            layout.insert(LayoutInsertion {
                window,
                space: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
            prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        }

        assert!(layout.move_beside(&1, &2, LayoutDirection::Down));
        assert!(layout.move_beside(&3, &2, LayoutDirection::Down));
        prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        let column = &layout.rows[&OUTPUT].columns[0];
        assert_eq!(
            column
                .tiles
                .iter()
                .map(|tile| tile.window)
                .collect::<Vec<_>>(),
            vec![2, 3, 1],
        );
        let placements = layout.arrange(OUTPUT, work_area, 10);
        let heights = [2, 3, 1].map(|window| {
            placements
                .iter()
                .find(|placement| placement.window == window)
                .expect("stacked tile remains arranged")
                .geometry
                .size
                .h
        });
        assert_eq!(heights, [145, 145, 290]);
    }

    #[test]
    fn scrolling_main_axis_drop_extracts_a_stacked_tile_as_a_column() {
        let mut layout = ScrollingLayout::<u64>::default();
        let work_area = rect(0, 0, 1000, 600);
        for window in 1..=3 {
            layout.insert(LayoutInsertion {
                window,
                space: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
            prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        }

        assert!(layout.move_beside(&1, &2, LayoutDirection::Down));
        assert!(layout.move_beside(&1, &3, LayoutDirection::Right));
        let row = &layout.rows[&OUTPUT];
        assert_eq!(row.columns.len(), 3);
        assert_eq!(
            row.columns
                .iter()
                .map(|column| column.tiles[0].window)
                .collect::<Vec<_>>(),
            vec![2, 3, 1],
        );
        assert!(row.columns.iter().all(|column| column.tiles.len() == 1));
    }

    #[test]
    fn scrolling_cross_axis_resize_moves_only_the_shared_boundary() {
        let mut layout = ScrollingLayout::<u64>::default();
        let work_area = rect(0, 0, 1000, 600);
        for window in 1..=2 {
            layout.insert(LayoutInsertion {
                window,
                space: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
            prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        }
        assert!(layout.move_beside(&1, &2, LayoutDirection::Down));
        prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);

        assert!(layout.resize(LayoutResizeRequest {
            window: 2,
            work_area,
            gap: 10,
            delta_x: 0.0,
            delta_y: 60.0,
            edges: LayoutResizeEdges {
                bottom: true,
                ..LayoutResizeEdges::default()
            },
        }));
        let placements = layout.arrange(OUTPUT, work_area, 10);
        assert_eq!(placements[0].window, 2);
        assert_eq!(placements[0].geometry.size.h, 355);
        assert_eq!(placements[1].window, 1);
        assert_eq!(placements[1].geometry.size.h, 235);
    }

    #[test]
    fn scrolling_rotated_strip_stacks_on_the_physical_cross_axis() {
        let mut layout = ScrollingLayout::<u64>::default();
        let work_area = rect(100, 20, 600, 1000);
        for window in 1..=2 {
            layout.insert(LayoutInsertion {
                window,
                space: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
            prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Vertical);
        }

        assert!(layout.move_beside(&1, &2, LayoutDirection::Left));
        prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Vertical);
        let placements = layout.arrange(OUTPUT, work_area, 10);
        assert_eq!(layout.rows[&OUTPUT].columns.len(), 1);
        assert_eq!(placements[0].window, 1);
        assert_eq!(placements[1].window, 2);
        assert_eq!(placements[0].geometry.size.w, 295);
        assert_eq!(placements[1].geometry.size.w, 295);
        assert_eq!(placements[0].geometry.loc.y, placements[1].geometry.loc.y);
    }

    #[test]
    fn scrolling_rebuild_preserves_multi_window_columns() {
        let mut layout = ScrollingLayout::<u64>::default();
        layout.insert(LayoutInsertion {
            window: 1,
            space: OUTPUT,
            anchor: None,
        });
        layout.insert(LayoutInsertion {
            window: 2,
            space: OUTPUT,
            anchor: Some(1),
        });
        assert!(layout.move_beside(&1, &2, LayoutDirection::Down));

        layout.rebuild(vec![
            LayoutInsertion {
                window: 1,
                space: OUTPUT,
                anchor: None,
            },
            LayoutInsertion {
                window: 2,
                space: OUTPUT,
                anchor: Some(1),
            },
        ]);
        let row = &layout.rows[&OUTPUT];
        assert_eq!(row.columns.len(), 1);
        assert_eq!(
            row.columns[0]
                .tiles
                .iter()
                .map(|tile| tile.window)
                .collect::<Vec<_>>(),
            vec![2, 1],
        );
    }

    #[test]
    fn scrolling_stack_preserves_impossible_cross_axis_minimums() {
        let mut layout = ScrollingLayout::<u64>::default();
        let work_area = rect(0, 0, 1000, 600);
        layout.insert(LayoutInsertion {
            window: 1,
            space: OUTPUT,
            anchor: None,
        });
        layout.insert(LayoutInsertion {
            window: 2,
            space: OUTPUT,
            anchor: Some(1),
        });
        assert!(layout.update_minimum_size(&1, Size::from((1, 400))));
        assert!(layout.update_minimum_size(&2, Size::from((1, 300))));
        prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);

        assert!(layout.move_beside(&1, &2, LayoutDirection::Down));
        prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        let placements = layout.arrange(OUTPUT, work_area, 10);
        assert_eq!(placements[0].window, 2);
        assert_eq!(placements[0].geometry.size.h, 300);
        assert_eq!(placements[1].window, 1);
        assert_eq!(placements[1].geometry.size.h, 400);
    }

    #[test]
    fn scrolling_cross_space_stack_preserves_source_and_minimum_metadata() {
        let mut layout = ScrollingLayout::<u64>::default();
        let work_area = rect(0, 0, 1000, 600);
        layout.insert(LayoutInsertion {
            window: 1,
            space: OUTPUT,
            anchor: None,
        });
        layout.insert(LayoutInsertion {
            window: 2,
            space: OUTPUT,
            anchor: Some(1),
        });
        layout.insert(LayoutInsertion {
            window: 3,
            space: SECOND_OUTPUT,
            anchor: None,
        });
        assert!(layout.update_minimum_size(&1, Size::from((320, 240))));
        prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        layout.prepare_arrange(SECOND_OUTPUT, work_area, 10, LayoutAxis::Horizontal);
        assert!(layout.move_beside(&1, &2, LayoutDirection::Down));

        assert!(layout.move_beside(&1, &3, LayoutDirection::Down));
        assert_eq!(layout.space_for(&1), Some(SECOND_OUTPUT));
        assert_eq!(layout.rows[&OUTPUT].columns[0].tiles[0].window, 2,);
        assert_eq!(
            layout.rows[&SECOND_OUTPUT].columns[0]
                .tiles
                .iter()
                .map(|tile| tile.window)
                .collect::<Vec<_>>(),
            vec![3, 1],
        );
        assert_eq!(
            layout_minimum_size(&layout.minimum_sizes, &1),
            Size::from((320, 240)),
        );
    }

    #[test]
    fn scrolling_reveals_inserted_and_activated_tiles_with_minimum_viewport_motion() {
        let mut layout = ScrollingLayout::<u64>::default();
        let work_area = rect(100, 20, 1000, 600);
        for window in 1..=3 {
            layout.insert(LayoutInsertion {
                window,
                space: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
            prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        }

        assert_eq!(
            layout.arrange(OUTPUT, work_area, 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(-720, 20, 600, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(-110, 20, 600, 600),
                },
                LayoutPlacement {
                    window: 3,
                    geometry: rect(500, 20, 600, 600),
                },
            ]
        );

        assert!(layout.activate(&2));
        prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        assert_eq!(
            layout.arrange(OUTPUT, work_area, 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(-510, 20, 600, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(100, 20, 600, 600),
                },
                LayoutPlacement {
                    window: 3,
                    geometry: rect(710, 20, 600, 600),
                },
            ]
        );

        layout.rebuild(
            (1..=3)
                .rev()
                .map(|window| LayoutInsertion {
                    window,
                    space: OUTPUT,
                    anchor: (window < 3).then_some(window + 1),
                })
                .collect(),
        );
        prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        assert_eq!(
            layout.arrange(OUTPUT, work_area, 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(-510, 20, 600, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(100, 20, 600, 600),
                },
                LayoutPlacement {
                    window: 3,
                    geometry: rect(710, 20, 600, 600),
                },
            ]
        );
    }

    #[test]
    fn scrolling_cross_space_swap_keeps_active_ids_in_their_rows() {
        let mut layout = ScrollingLayout::<u64>::default();
        layout.insert(LayoutInsertion {
            window: 1,
            space: OUTPUT,
            anchor: None,
        });
        layout.insert(LayoutInsertion {
            window: 2,
            space: SECOND_OUTPUT,
            anchor: None,
        });

        assert!(layout.swap(&1, &2));
        assert_eq!(layout.space_for(&1), Some(SECOND_OUTPUT));
        assert_eq!(layout.space_for(&2), Some(OUTPUT));
        for row in layout.rows.values() {
            assert!(
                row.active
                    .as_ref()
                    .is_some_and(|active| row.position(active).is_some())
            );
        }
        assert!(layout.activate(&1) || layout.rows[&SECOND_OUTPUT].active == Some(1));
        assert_eq!(layout.rows[&SECOND_OUTPUT].active, Some(1));
    }

    #[test]
    fn scrolling_keeps_every_tile_inside_once_their_existing_sizes_fit() {
        let mut layout = ScrollingLayout::<u64>::default();
        let work_area = rect(0, 0, 1000, 600);
        for window in 1..=2 {
            layout.insert(LayoutInsertion {
                window,
                space: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
            prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        }
        for window in 1..=2 {
            assert!(layout.resize(LayoutResizeRequest {
                window,
                work_area,
                gap: 10,
                delta_x: -200.0,
                delta_y: 0.0,
                edges: LayoutResizeEdges {
                    right: true,
                    ..LayoutResizeEdges::default()
                },
            }));
            prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        }

        assert_eq!(
            layout.arrange(OUTPUT, work_area, 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(0, 0, 400, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(410, 0, 400, 600),
                },
            ]
        );
    }

    #[test]
    fn scrolling_uses_full_width_tiles_on_a_vertical_axis() {
        let mut layout = ScrollingLayout::<u64>::default();
        let work_area = rect(100, 20, 600, 1000);
        for window in 1..=3 {
            layout.insert(LayoutInsertion {
                window,
                space: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
            prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Vertical);
        }

        assert_eq!(
            layout.arrange(OUTPUT, work_area, 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(100, -800, 600, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(100, -190, 600, 600),
                },
                LayoutPlacement {
                    window: 3,
                    geometry: rect(100, 420, 600, 600),
                },
            ]
        );
    }

    #[test]
    fn scrolling_resize_changes_only_the_selected_column_width() {
        let mut layout = ScrollingLayout::<u64>::default();
        let work_area = rect(0, 0, 1000, 600);
        for window in 1..=2 {
            layout.insert(LayoutInsertion {
                window,
                space: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
            prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        }

        assert!(layout.resize(LayoutResizeRequest {
            window: 2,
            work_area,
            gap: 10,
            delta_x: 100.0,
            delta_y: 0.0,
            edges: LayoutResizeEdges {
                right: true,
                ..LayoutResizeEdges::default()
            },
        }));
        prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        assert_eq!(
            layout.arrange(OUTPUT, work_area, 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(-310, 0, 600, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(300, 0, 700, 600),
                },
            ]
        );

        layout.rebuild(vec![
            LayoutInsertion {
                window: 2,
                space: OUTPUT,
                anchor: None,
            },
            LayoutInsertion {
                window: 1,
                space: OUTPUT,
                anchor: Some(2),
            },
        ]);
        prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        assert_eq!(
            layout.arrange(OUTPUT, work_area, 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(-310, 0, 600, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(300, 0, 700, 600),
                },
            ]
        );
    }

    #[test]
    fn scrolling_gesture_tracks_motion_and_settles_to_nearest_column() {
        let mut layout = ScrollingLayout::<u64>::default();
        let work_area = rect(0, 0, 1000, 600);
        for window in 1..=3 {
            layout.insert(LayoutInsertion {
                window,
                space: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
            prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        }
        assert!(layout.activate(&2));

        assert!(layout.scroll_horizontally(OUTPUT, work_area, 10, LayoutAxis::Horizontal, -400.0,));
        assert_eq!(
            layout.arrange(OUTPUT, rect(0, 0, 1000, 600), 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(-820, 0, 600, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(-210, 0, 600, 600),
                },
                LayoutPlacement {
                    window: 3,
                    geometry: rect(400, 0, 600, 600),
                },
            ]
        );
        assert_eq!(
            layout.finish_horizontal_scroll(
                OUTPUT,
                work_area,
                10,
                LayoutAxis::Horizontal,
                false,
                None,
            ),
            Some(3)
        );
        assert_eq!(
            layout.arrange(OUTPUT, rect(0, 0, 1000, 600), 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(-820, 0, 600, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(-210, 0, 600, 600),
                },
                LayoutPlacement {
                    window: 3,
                    geometry: rect(400, 0, 600, 600),
                },
            ]
        );
    }

    #[test]
    fn scrolling_flick_is_capped_to_one_column_despite_high_velocity() {
        let mut layout = ScrollingLayout::<u64>::default();
        let work_area = rect(0, 0, 1000, 600);
        for window in 1..=7 {
            layout.insert(LayoutInsertion {
                window,
                space: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
            prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        }
        assert!(layout.activate(&4));

        // The tracked distance alone is below the nearest-column threshold,
        // while an intentionally extreme release projection crosses several.
        assert!(layout.scroll_horizontally(OUTPUT, work_area, 10, LayoutAxis::Horizontal, -100.0,));
        assert_eq!(
            layout.finish_horizontal_scroll(
                OUTPUT,
                work_area,
                10,
                LayoutAxis::Horizontal,
                false,
                Some(-10_000.0),
            ),
            Some(5)
        );
    }

    #[test]
    fn continued_scrolling_settles_by_travel_without_extra_fling() {
        let mut layout = ScrollingLayout::<u64>::default();
        let work_area = rect(0, 0, 1000, 600);
        for window in 1..=5 {
            layout.insert(LayoutInsertion {
                window,
                space: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
            prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        }
        assert!(layout.activate(&2));

        assert!(layout.scroll_horizontally(
            OUTPUT,
            work_area,
            10,
            LayoutAxis::Horizontal,
            -1_300.0,
        ));
        assert_eq!(
            layout.finish_horizontal_scroll(
                OUTPUT,
                work_area,
                10,
                LayoutAxis::Horizontal,
                false,
                Some(-10_000.0),
            ),
            Some(4)
        );
    }

    #[test]
    fn cancelled_scrolling_gesture_returns_to_original_column() {
        let mut layout = ScrollingLayout::<u64>::default();
        let work_area = rect(0, 0, 1000, 600);
        for window in 1..=2 {
            layout.insert(LayoutInsertion {
                window,
                space: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
            prepare_scrolling(&mut layout, work_area, 10, LayoutAxis::Horizontal);
        }

        assert!(layout.scroll_horizontally(OUTPUT, work_area, 10, LayoutAxis::Horizontal, 250.0,));
        assert_eq!(
            layout.finish_horizontal_scroll(
                OUTPUT,
                work_area,
                10,
                LayoutAxis::Horizontal,
                true,
                None,
            ),
            Some(2)
        );
        assert_eq!(
            layout.arrange(OUTPUT, rect(0, 0, 1000, 600), 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(-210, 0, 600, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(400, 0, 600, 600),
                },
            ]
        );
    }

    #[test]
    fn scrolling_removal_returns_focus_to_the_left_neighbor() {
        let mut layout = ScrollingLayout::<u64>::default();
        let work_area = rect(0, 0, 1000, 600);
        for window in 1..=3 {
            layout.insert(LayoutInsertion {
                window,
                space: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
            prepare_scrolling(&mut layout, work_area, 0, LayoutAxis::Horizontal);
        }

        assert!(layout.remove(&3));
        prepare_scrolling(&mut layout, work_area, 0, LayoutAxis::Horizontal);
        assert_eq!(
            layout.arrange(OUTPUT, rect(0, 0, 1000, 600), 0),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(-200, 0, 600, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(400, 0, 600, 600),
                },
            ]
        );
    }
}
