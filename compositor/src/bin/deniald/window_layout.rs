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
    pub(super) output: OutputId,
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
    WindowId: Clone + Eq,
{
    fn kind(&self) -> WindowLayoutKind;

    /// Stacking leaves geometry under the existing free-placement policy.
    fn manages_geometry(&self) -> bool {
        true
    }

    fn insert(&mut self, insertion: LayoutInsertion<WindowId>);
    fn remove(&mut self, window: &WindowId) -> bool;
    fn contains(&self, window: &WindowId) -> bool;
    fn clear(&mut self);

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

    /// Adjust layout-owned geometry for an interactive resize. The request is
    /// deliberately expressed without compositor or protocol types so a new
    /// layout can implement its own size policy without touching input code.
    fn resize(&mut self, _request: LayoutResizeRequest<WindowId>) -> bool {
        false
    }

    /// Translate a layout-owned horizontal viewport by one gesture delta.
    /// Fixed layouts keep the default no-op.
    fn scroll_horizontally(
        &mut self,
        _output: OutputId,
        _work_area: Rectangle<i32, Logical>,
        _gap: i32,
        _delta_x: f64,
    ) -> bool {
        false
    }

    /// Settle a translated viewport, optionally cancelling back to its
    /// original active leaf. The returned leaf should receive keyboard focus.
    fn finish_horizontal_scroll(
        &mut self,
        _output: OutputId,
        _work_area: Rectangle<i32, Logical>,
        _gap: i32,
        _cancelled: bool,
        _projected_translation: Option<f64>,
    ) -> Option<WindowId> {
        None
    }

    /// Arrange one output. `gap` is the logical distance between siblings;
    /// outer insets are already reflected in `work_area`.
    fn arrange(
        &self,
        output: OutputId,
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

#[derive(Debug, Default)]
struct StackingLayout;

impl<WindowId> WindowLayout<WindowId> for StackingLayout
where
    WindowId: Clone + Eq,
{
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

    fn clear(&mut self) {}

    fn arrange(
        &self,
        _output: OutputId,
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
/// output rotation and resizing naturally recompute the tree without storing
/// stale axes. Removal collapses the now-single-child parent.
#[derive(Debug)]
struct DwindleLayout<WindowId> {
    roots: HashMap<OutputId, DwindleNode<WindowId>>,
}

impl<WindowId> Default for DwindleLayout<WindowId> {
    fn default() -> Self {
        Self {
            roots: HashMap::new(),
        }
    }
}

#[derive(Debug)]
enum DwindleNode<WindowId> {
    Window(WindowId),
    Split {
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
                ratio,
                first,
                second,
            } => {
                let (first, removed) = first.remove(window);
                if removed {
                    return match first {
                        Some(first) => (
                            Some(Self::Split {
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

    fn arrange(
        &self,
        geometry: Rectangle<i32, Logical>,
        gap: i32,
        placements: &mut Vec<LayoutPlacement<WindowId>>,
    ) {
        match self {
            Self::Window(window) => placements.push(LayoutPlacement {
                window: window.clone(),
                geometry,
            }),
            Self::Split {
                ratio,
                first,
                second,
            } => {
                let (first_geometry, second_geometry) = split_geometry(geometry, gap, *ratio);
                first.arrange(first_geometry, gap, placements);
                second.arrange(second_geometry, gap, placements);
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

        let (first_geometry, second_geometry) = split_geometry(geometry, gap, *ratio);
        let mut resized = if window_in_first {
            first.resize_window(window, first_geometry, gap, edges, delta_x, delta_y)
        } else {
            second.resize_window(window, second_geometry, gap, edges, delta_x, delta_y)
        };

        let horizontal_split = geometry.size.w >= geometry.size.h;
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
    WindowId: Clone + Debug + Eq,
{
    fn kind(&self) -> WindowLayoutKind {
        WindowLayoutKind::Dwindle
    }

    fn insert(&mut self, insertion: LayoutInsertion<WindowId>) {
        self.remove(&insertion.window);
        let Some(root) = self.roots.get_mut(&insertion.output) else {
            self.roots
                .insert(insertion.output, DwindleNode::Window(insertion.window));
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
        let output = self
            .roots
            .iter()
            .find_map(|(output, root)| root.contains(window).then_some(*output));
        let Some(output) = output else {
            return false;
        };
        let root = self
            .roots
            .remove(&output)
            .expect("located dwindle output must exist");
        let (root, removed) = root.remove(window);
        if let Some(root) = root {
            self.roots.insert(output, root);
        }
        removed
    }

    fn contains(&self, window: &WindowId) -> bool {
        self.roots.values().any(|root| root.contains(window))
    }

    fn clear(&mut self) {
        self.roots.clear();
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
        output: OutputId,
        work_area: Rectangle<i32, Logical>,
        gap: i32,
    ) -> Vec<LayoutPlacement<WindowId>> {
        let Some(root) = self.roots.get(&output) else {
            return Vec::new();
        };
        let mut placements = Vec::new();
        root.arrange(work_area, gap.max(0), &mut placements);
        placements
    }
}

/// A focus-following horizontal strip of full-height columns.
///
/// The layout borrows the infinite horizontal workspace idea from niri, but
/// gives it a Denial-specific rhythm: every new column starts at three fifths
/// of the work area and the active column stays centered. Neighboring columns
/// remain visible at the sides, making the available direction apparent
/// without requiring a separate overview mode.
#[derive(Debug)]
struct ScrollingLayout<WindowId> {
    rows: HashMap<OutputId, ScrollingRow<WindowId>>,
}

impl<WindowId> Default for ScrollingLayout<WindowId> {
    fn default() -> Self {
        Self {
            rows: HashMap::new(),
        }
    }
}

#[derive(Debug)]
struct ScrollingRow<WindowId> {
    columns: Vec<ScrollingColumn<WindowId>>,
    active: Option<WindowId>,
    scroll_translation: f64,
}

#[derive(Debug)]
struct ScrollingColumn<WindowId> {
    window: WindowId,
    width_fraction: f64,
}

pub(super) const DEFAULT_SCROLLING_COLUMN_FRACTION: f64 = 3.0 / 5.0;
const MIN_SCROLLING_COLUMN_FRACTION: f64 = 1.0 / 4.0;

impl<WindowId> ScrollingRow<WindowId>
where
    WindowId: Clone + Eq,
{
    fn position(&self, window: &WindowId) -> Option<usize> {
        self.columns
            .iter()
            .position(|column| &column.window == window)
    }

    fn widths(&self, work_width: i32) -> Vec<i64> {
        self.columns
            .iter()
            .map(|column| {
                (f64::from(work_width) * column.width_fraction)
                    .round()
                    .clamp(1.0, f64::from(work_width)) as i64
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
            .and_then(|active| self.position(active))
            .unwrap_or_else(|| self.columns.len().saturating_sub(1))
    }

    fn scroll_horizontally(&mut self, work_width: i32, gap: i32, delta_x: f64) -> bool {
        if self.columns.len() < 2 || !delta_x.is_finite() || delta_x == 0.0 {
            return false;
        }
        let centers = Self::centers(&self.widths(work_width), gap);
        let active_center = centers[self.active_index()];
        let minimum = active_center - centers[centers.len() - 1];
        let maximum = active_center - centers[0];
        let next = (self.scroll_translation + delta_x).clamp(minimum, maximum);
        if (next - self.scroll_translation).abs() < f64::EPSILON {
            return false;
        }
        self.scroll_translation = next;
        true
    }

    fn finish_horizontal_scroll(
        &mut self,
        work_width: i32,
        gap: i32,
        cancelled: bool,
        projected_translation: Option<f64>,
    ) -> Option<WindowId> {
        if self.columns.is_empty() {
            return None;
        }
        let active_index = self.active_index();
        let selected_index = if cancelled {
            active_index
        } else {
            let centers = Self::centers(&self.widths(work_width), gap);
            let nearest_index = |translation: f64| {
                let viewport_center = centers[active_index] - translation;
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
            let tracked_index = nearest_index(self.scroll_translation);
            if tracked_index != active_index {
                // Continued travel is authoritative: settle on the column the
                // fingers actually reached instead of adding fling distance.
                tracked_index
            } else if let Some(projected_translation) =
                projected_translation.filter(|translation| translation.is_finite())
            {
                // Momentum exists only to turn a short flick into one step.
                // Never let a high release velocity skip several columns.
                match nearest_index(projected_translation).cmp(&active_index) {
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
        let selected = self.columns[selected_index].window.clone();
        self.active = Some(selected.clone());
        self.scroll_translation = 0.0;
        Some(selected)
    }

    fn arrange(
        &self,
        work_area: Rectangle<i32, Logical>,
        requested_gap: i32,
    ) -> Vec<LayoutPlacement<WindowId>> {
        if self.columns.is_empty() {
            return Vec::new();
        }

        let work_width = work_area.size.w.max(1);
        let gap = requested_gap.max(0);
        let widths = self.widths(work_width);
        let active_idx = self.active_index();
        let active_start = widths.iter().take(active_idx).fold(0_i64, |x, width| {
            x.saturating_add(*width).saturating_add(i64::from(gap))
        });
        let viewport_center = i64::from(work_width) / 2;
        let active_center = active_start.saturating_add(widths[active_idx] / 2);
        let view_start =
            active_center.saturating_sub(viewport_center) as f64 - self.scroll_translation;

        let mut column_start = 0_i64;
        self.columns
            .iter()
            .zip(widths)
            .map(|(column, width)| {
                let x = (f64::from(work_area.loc.x) + column_start as f64 - view_start)
                    .round()
                    .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
                let width = width.clamp(1, i64::from(i32::MAX)) as i32;
                column_start = column_start
                    .saturating_add(i64::from(width))
                    .saturating_add(i64::from(gap));
                LayoutPlacement {
                    window: column.window.clone(),
                    geometry: Rectangle::new(
                        Point::from((x, work_area.loc.y)),
                        Size::from((width, work_area.size.h.max(1))),
                    ),
                }
            })
            .collect()
    }
}

impl<WindowId> WindowLayout<WindowId> for ScrollingLayout<WindowId>
where
    WindowId: Clone + Debug + Eq,
{
    fn kind(&self) -> WindowLayoutKind {
        WindowLayoutKind::Scrolling
    }

    fn insert(&mut self, insertion: LayoutInsertion<WindowId>) {
        self.remove(&insertion.window);
        let row = self
            .rows
            .entry(insertion.output)
            .or_insert_with(|| ScrollingRow {
                columns: Vec::new(),
                active: None,
                scroll_translation: 0.0,
            });
        let index = insertion
            .anchor
            .as_ref()
            .and_then(|anchor| row.position(anchor))
            .map_or(row.columns.len(), |anchor| anchor + 1);
        row.columns.insert(
            index,
            ScrollingColumn {
                window: insertion.window.clone(),
                width_fraction: DEFAULT_SCROLLING_COLUMN_FRACTION,
            },
        );
        row.active = Some(insertion.window);
        row.scroll_translation = 0.0;
    }

    fn remove(&mut self, window: &WindowId) -> bool {
        let output = self
            .rows
            .iter()
            .find_map(|(output, row)| row.position(window).map(|index| (*output, index)));
        let Some((output, index)) = output else {
            return false;
        };
        let row = self.rows.get_mut(&output).expect("located scrolling row");
        let was_active = row.active.as_ref() == Some(window);
        row.columns.remove(index);
        row.scroll_translation = 0.0;
        if row.columns.is_empty() {
            self.rows.remove(&output);
        } else if was_active {
            let next_active = index.checked_sub(1).unwrap_or(0).min(row.columns.len() - 1);
            row.active = Some(row.columns[next_active].window.clone());
        }
        true
    }

    fn contains(&self, window: &WindowId) -> bool {
        self.rows.values().any(|row| row.position(window).is_some())
    }

    fn clear(&mut self) {
        self.rows.clear();
    }

    fn rebuild(&mut self, insertions: Vec<LayoutInsertion<WindowId>>) {
        let previous_rows = std::mem::take(&mut self.rows);
        let previous_widths = previous_rows
            .values()
            .flat_map(|row| &row.columns)
            .map(|column| (column.window.clone(), column.width_fraction))
            .collect::<Vec<_>>();

        for (output, previous) in previous_rows {
            let previous_active_index = previous
                .active
                .as_ref()
                .and_then(|active| previous.position(active));
            let columns = previous
                .columns
                .into_iter()
                .filter(|column| {
                    insertions.iter().any(|insertion| {
                        insertion.output == output && insertion.window == column.window
                    })
                })
                .collect::<Vec<_>>();
            if columns.is_empty() {
                continue;
            }
            let active = previous
                .active
                .filter(|active| columns.iter().any(|column| &column.window == active))
                .or_else(|| {
                    previous_active_index.and_then(|index| {
                        columns
                            .get(index.min(columns.len() - 1))
                            .map(|column| column.window.clone())
                    })
                });
            self.rows.insert(
                output,
                ScrollingRow {
                    columns,
                    active,
                    scroll_translation: 0.0,
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
                .entry(insertion.output)
                .or_insert_with(|| ScrollingRow {
                    columns: Vec::new(),
                    active: None,
                    scroll_translation: 0.0,
                });
            let index = insertion
                .anchor
                .as_ref()
                .and_then(|anchor| row.position(anchor))
                .map_or(row.columns.len(), |anchor| anchor + 1);
            row.columns.insert(
                index,
                ScrollingColumn {
                    window: insertion.window.clone(),
                    width_fraction,
                },
            );
            row.active.get_or_insert(insertion.window);
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
        if row.active.as_ref() == Some(window) && row.scroll_translation == 0.0 {
            return false;
        }
        row.active = Some(window.clone());
        row.scroll_translation = 0.0;
        true
    }

    fn swap(&mut self, first: &WindowId, second: &WindowId) -> bool {
        if first == second || !self.contains(first) || !self.contains(second) {
            return false;
        }
        for row in self.rows.values_mut() {
            for column in &mut row.columns {
                if &column.window == first {
                    column.window = second.clone();
                } else if &column.window == second {
                    column.window = first.clone();
                }
            }
        }
        true
    }

    fn resize(&mut self, request: LayoutResizeRequest<WindowId>) -> bool {
        if !request.delta_x.is_finite()
            || request.work_area.size.w <= 0
            || (!request.edges.left && !request.edges.right)
        {
            return false;
        }
        let Some(column) = self
            .rows
            .values_mut()
            .flat_map(|row| &mut row.columns)
            .find(|column| column.window == request.window)
        else {
            return false;
        };
        let signed_delta = if request.edges.left && !request.edges.right {
            -request.delta_x
        } else {
            request.delta_x
        };
        let next = (column.width_fraction + signed_delta / f64::from(request.work_area.size.w))
            .clamp(MIN_SCROLLING_COLUMN_FRACTION, 1.0);
        if (next - column.width_fraction).abs() < f64::EPSILON {
            return false;
        }
        column.width_fraction = next;
        true
    }

    fn scroll_horizontally(
        &mut self,
        output: OutputId,
        work_area: Rectangle<i32, Logical>,
        gap: i32,
        delta_x: f64,
    ) -> bool {
        self.rows.get_mut(&output).is_some_and(|row| {
            row.scroll_horizontally(work_area.size.w.max(1), gap.max(0), delta_x)
        })
    }

    fn finish_horizontal_scroll(
        &mut self,
        output: OutputId,
        work_area: Rectangle<i32, Logical>,
        gap: i32,
        cancelled: bool,
        projected_translation: Option<f64>,
    ) -> Option<WindowId> {
        self.rows.get_mut(&output)?.finish_horizontal_scroll(
            work_area.size.w.max(1),
            gap.max(0),
            cancelled,
            projected_translation,
        )
    }

    fn arrange(
        &self,
        output: OutputId,
        work_area: Rectangle<i32, Logical>,
        gap: i32,
    ) -> Vec<LayoutPlacement<WindowId>> {
        self.rows
            .get(&output)
            .map_or_else(Vec::new, |row| row.arrange(work_area, gap))
    }
}

fn split_geometry(
    geometry: Rectangle<i32, Logical>,
    requested_gap: i32,
    requested_ratio: f64,
) -> (Rectangle<i32, Logical>, Rectangle<i32, Logical>) {
    let horizontal = geometry.size.w >= geometry.size.h;
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
    let first_extent = (f64::from(available) * ratio).round() as i32;
    let first_extent = first_extent.clamp(1, available.saturating_sub(1).max(1));
    let second_extent = available.saturating_sub(first_extent).max(1);

    if horizontal {
        (
            Rectangle::new(geometry.loc, Size::from((first_extent, geometry.size.h))),
            Rectangle::new(
                Point::from((
                    geometry
                        .loc
                        .x
                        .saturating_add(first_extent)
                        .saturating_add(gap),
                    geometry.loc.y,
                )),
                Size::from((second_extent, geometry.size.h)),
            ),
        )
    } else {
        (
            Rectangle::new(geometry.loc, Size::from((geometry.size.w, first_extent))),
            Rectangle::new(
                Point::from((
                    geometry.loc.x,
                    geometry
                        .loc
                        .y
                        .saturating_add(first_extent)
                        .saturating_add(gap),
                )),
                Size::from((geometry.size.w, second_extent)),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTPUT: OutputId = OutputId(1);

    fn rect(x: i32, y: i32, width: i32, height: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((width, height)))
    }

    #[test]
    fn dwindle_splits_the_focused_leaf_and_uses_parent_aspect_ratio() {
        let mut layout = DwindleLayout::<u64>::default();
        layout.insert(LayoutInsertion {
            window: 1,
            output: OUTPUT,
            anchor: None,
        });
        layout.insert(LayoutInsertion {
            window: 2,
            output: OUTPUT,
            anchor: Some(1),
        });
        layout.insert(LayoutInsertion {
            window: 3,
            output: OUTPUT,
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
    fn removing_a_leaf_collapses_its_parent_without_disturbing_other_outputs() {
        let mut layout = DwindleLayout::<u64>::default();
        for window in 1..=3 {
            layout.insert(LayoutInsertion {
                window,
                output: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
        }
        layout.insert(LayoutInsertion {
            window: 4,
            output: OutputId(2),
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
            layout.arrange(OutputId(2), rect(800, 0, 800, 600), 0),
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
            output: OUTPUT,
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
                output: OUTPUT,
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
                output: OUTPUT,
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
    fn scrolling_inserts_after_focus_and_retains_the_active_viewport() {
        let mut layout = ScrollingLayout::<u64>::default();
        for window in 1..=3 {
            layout.insert(LayoutInsertion {
                window,
                output: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
        }

        assert_eq!(
            layout.arrange(OUTPUT, rect(100, 20, 1000, 600), 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(-920, 20, 600, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(-310, 20, 600, 600),
                },
                LayoutPlacement {
                    window: 3,
                    geometry: rect(300, 20, 600, 600),
                },
            ]
        );

        assert!(layout.activate(&2));
        assert_eq!(
            layout.arrange(OUTPUT, rect(100, 20, 1000, 600), 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(-310, 20, 600, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(300, 20, 600, 600),
                },
                LayoutPlacement {
                    window: 3,
                    geometry: rect(910, 20, 600, 600),
                },
            ]
        );

        layout.rebuild(
            (1..=3)
                .rev()
                .map(|window| LayoutInsertion {
                    window,
                    output: OUTPUT,
                    anchor: (window < 3).then_some(window + 1),
                })
                .collect(),
        );
        assert_eq!(
            layout.arrange(OUTPUT, rect(100, 20, 1000, 600), 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(-310, 20, 600, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(300, 20, 600, 600),
                },
                LayoutPlacement {
                    window: 3,
                    geometry: rect(910, 20, 600, 600),
                },
            ]
        );
    }

    #[test]
    fn scrolling_resize_changes_only_the_selected_column_width() {
        let mut layout = ScrollingLayout::<u64>::default();
        for window in 1..=2 {
            layout.insert(LayoutInsertion {
                window,
                output: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
        }

        assert!(layout.resize(LayoutResizeRequest {
            window: 2,
            work_area: rect(0, 0, 1000, 600),
            gap: 10,
            delta_x: 100.0,
            delta_y: 0.0,
            edges: LayoutResizeEdges {
                right: true,
                ..LayoutResizeEdges::default()
            },
        }));
        assert_eq!(
            layout.arrange(OUTPUT, rect(0, 0, 1000, 600), 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(-460, 0, 600, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(150, 0, 700, 600),
                },
            ]
        );

        layout.rebuild(vec![
            LayoutInsertion {
                window: 2,
                output: OUTPUT,
                anchor: None,
            },
            LayoutInsertion {
                window: 1,
                output: OUTPUT,
                anchor: Some(2),
            },
        ]);
        assert_eq!(
            layout.arrange(OUTPUT, rect(0, 0, 1000, 600), 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(-460, 0, 600, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(150, 0, 700, 600),
                },
            ]
        );
    }

    #[test]
    fn scrolling_gesture_tracks_motion_and_settles_to_nearest_column() {
        let mut layout = ScrollingLayout::<u64>::default();
        for window in 1..=3 {
            layout.insert(LayoutInsertion {
                window,
                output: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
        }
        assert!(layout.activate(&2));

        assert!(layout.scroll_horizontally(OUTPUT, rect(0, 0, 1000, 600), 10, -400.0,));
        assert_eq!(
            layout.arrange(OUTPUT, rect(0, 0, 1000, 600), 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(-810, 0, 600, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(-200, 0, 600, 600),
                },
                LayoutPlacement {
                    window: 3,
                    geometry: rect(410, 0, 600, 600),
                },
            ]
        );
        assert_eq!(
            layout.finish_horizontal_scroll(OUTPUT, rect(0, 0, 1000, 600), 10, false, None,),
            Some(3)
        );
        assert_eq!(
            layout.arrange(OUTPUT, rect(0, 0, 1000, 600), 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(-1020, 0, 600, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(-410, 0, 600, 600),
                },
                LayoutPlacement {
                    window: 3,
                    geometry: rect(200, 0, 600, 600),
                },
            ]
        );
    }

    #[test]
    fn scrolling_flick_is_capped_to_one_column_despite_high_velocity() {
        let mut layout = ScrollingLayout::<u64>::default();
        for window in 1..=7 {
            layout.insert(LayoutInsertion {
                window,
                output: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
        }
        assert!(layout.activate(&4));

        // The tracked distance alone is below the nearest-column threshold,
        // while an intentionally extreme release projection crosses several.
        assert!(layout.scroll_horizontally(OUTPUT, rect(0, 0, 1000, 600), 10, -100.0));
        assert_eq!(
            layout.finish_horizontal_scroll(
                OUTPUT,
                rect(0, 0, 1000, 600),
                10,
                false,
                Some(-10_000.0),
            ),
            Some(5)
        );
    }

    #[test]
    fn continued_scrolling_settles_by_travel_without_extra_fling() {
        let mut layout = ScrollingLayout::<u64>::default();
        for window in 1..=5 {
            layout.insert(LayoutInsertion {
                window,
                output: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
        }
        assert!(layout.activate(&2));

        assert!(layout.scroll_horizontally(OUTPUT, rect(0, 0, 1000, 600), 10, -1_300.0));
        assert_eq!(
            layout.finish_horizontal_scroll(
                OUTPUT,
                rect(0, 0, 1000, 600),
                10,
                false,
                Some(-10_000.0),
            ),
            Some(4)
        );
    }

    #[test]
    fn cancelled_scrolling_gesture_returns_to_original_column() {
        let mut layout = ScrollingLayout::<u64>::default();
        for window in 1..=2 {
            layout.insert(LayoutInsertion {
                window,
                output: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
        }

        assert!(layout.scroll_horizontally(OUTPUT, rect(0, 0, 1000, 600), 10, 250.0,));
        assert_eq!(
            layout.finish_horizontal_scroll(OUTPUT, rect(0, 0, 1000, 600), 10, true, None,),
            Some(2)
        );
        assert_eq!(
            layout.arrange(OUTPUT, rect(0, 0, 1000, 600), 10),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(-410, 0, 600, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(200, 0, 600, 600),
                },
            ]
        );
    }

    #[test]
    fn scrolling_removal_returns_focus_to_the_left_neighbor() {
        let mut layout = ScrollingLayout::<u64>::default();
        for window in 1..=3 {
            layout.insert(LayoutInsertion {
                window,
                output: OUTPUT,
                anchor: (window > 1).then_some(window - 1),
            });
        }

        assert!(layout.remove(&3));
        assert_eq!(
            layout.arrange(OUTPUT, rect(0, 0, 1000, 600), 0),
            vec![
                LayoutPlacement {
                    window: 1,
                    geometry: rect(-400, 0, 600, 600),
                },
                LayoutPlacement {
                    window: 2,
                    geometry: rect(200, 0, 600, 600),
                },
            ]
        );
    }
}
