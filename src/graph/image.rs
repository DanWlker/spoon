mod antialias;

use std::{
    borrow::Cow,
    fmt::{self, Debug, Formatter},
    hash::{Hash, Hasher},
    io::Cursor,
    ops::Range,
    process,
    time::{SystemTime, UNIX_EPOCH},
};

use rustc_hash::{FxHashMap, FxHashSet};

use crate::{
    color::GraphColorSet,
    git::CommitHash,
    graph::{
        geometry::{bounding_box_u32, Point, Vector},
        Edge, EdgeType, Fold, Graph,
    },
    protocol::{ImageProtocol, PreparedImage},
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphStyle {
    #[default]
    Rounded,
    Angular,
    Curved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GraphImageWidthMode {
    Compact,
    Fixed,
}

#[derive(Debug)]
pub struct GraphImageManager<'a> {
    // each with the fingerprint of the row image it was prepared from, see `fingerprint`
    prepared_image_map: FxHashMap<CommitHash, (u64, PreparedImage)>,
    // those of the manager before it, not checked against this graph yet, see `with_images`
    previous: FxHashMap<CommitHash, (u64, PreparedImage)>,
    image_ids: ImageIds,
    pending_uploads: Vec<String>,
    // the images of commits no longer shown, deleted with the next frame, see `with_images`;
    // deleted when the manager is dropped before it
    deleting: Vec<u32>,

    graph: Graph<'a>,
    cell_width_type: CellWidthType,
    graph_style: GraphStyle,
    image_width_mode: GraphImageWidthMode,
    image_params: ImageParams,
    drawing_pixels: DrawingPixels,
    image_protocol: ImageProtocol,
}

// The id of each commit's image in the terminal, its own for as long as the image is there: a hash
// of the commit, as before, was another commit's too about twice in 200 000. Counted from a random
// start, so two apps drawing in one terminal don't take each other's.
#[derive(Debug)]
struct ImageIds {
    ids: FxHashMap<CommitHash, u32>,
    next: u32,
}

impl ImageIds {
    fn new() -> Self {
        ImageIds {
            ids: FxHashMap::default(),
            next: create_session_nonce(),
        }
    }

    // The id of the commit's image, a new one the first time.
    fn of(&mut self, commit_hash: &CommitHash) -> u32 {
        if let Some(&id) = self.ids.get(commit_hash) {
            return id;
        }
        // kitty takes no image of id 0
        let id = self.next.max(1);
        self.next = id.wrapping_add(1);
        self.ids.insert(commit_hash.clone(), id);
        id
    }

    // The id of the commit's image deleted from the terminal, which it no longer has.
    fn remove(&mut self, commit_hash: &CommitHash) -> Option<u32> {
        self.ids.remove(commit_hash)
    }

    fn all(&self) -> Vec<u32> {
        let mut ids: Vec<u32> = self.ids.values().copied().collect();
        ids.sort_unstable();
        ids
    }
}

impl<'a> GraphImageManager<'a> {
    pub fn new(
        graph: Graph<'a>,
        image_params: ImageParams,
        cell_width_type: CellWidthType,
        graph_style: GraphStyle,
        image_width_mode: GraphImageWidthMode,
        image_protocol: ImageProtocol,
    ) -> Self {
        let drawing_pixels = DrawingPixels::new(&image_params);

        GraphImageManager {
            prepared_image_map: FxHashMap::default(),
            previous: FxHashMap::default(),
            image_ids: ImageIds::new(),
            pending_uploads: Vec::default(),
            deleting: Vec::default(),
            graph,
            cell_width_type,
            graph_style,
            image_width_mode,
            image_params,
            drawing_pixels,
            image_protocol,
        }
    }

    // Takes over the images of the manager before it, on a graph laid out or loaded again: the
    // rows drawn the same keep their images, already in the terminal, so they aren't drawn again.
    // Those of the commits no longer shown, folded or gone like after a rebase, are deleted with
    // the first frame, else they'd pile up in the terminal.
    pub fn with_images(mut self, images: GraphImages) -> Self {
        let GraphImages {
            mut images,
            mut image_ids,
            mut pending_uploads,
            mut deleting,
        } = images;
        images.retain(|commit_hash, _| {
            let shown = self.graph.commit_pos_map.contains_key(commit_hash);
            if !shown {
                if let Some(image_id) = image_ids.remove(commit_hash) {
                    pending_uploads.extend(self.image_protocol.delete_image(image_id));
                    deleting.push(image_id);
                }
            }
            shown
        });
        self.previous = images;
        self.image_ids = image_ids;
        self.pending_uploads = pending_uploads;
        self.deleting = deleting;
        self
    }

    // Its images, left in the terminal for the next manager, see `with_images`.
    // The graph's maps of every commit are freed on another thread: 100k take the screen ~10ms.
    pub fn into_images(mut self) -> GraphImages {
        let graph = &mut self.graph;
        crate::background::drop_elsewhere((
            std::mem::take(&mut graph.commit_pos_map),
            std::mem::take(&mut graph.edges),
            std::mem::take(&mut graph.merges),
            std::mem::take(&mut graph.folded),
        ));
        let mut images = std::mem::take(&mut self.previous);
        images.extend(self.prepared_image_map.drain());
        GraphImages {
            images,
            image_ids: std::mem::replace(&mut self.image_ids, ImageIds::new()),
            pending_uploads: std::mem::take(&mut self.pending_uploads),
            deleting: std::mem::take(&mut self.deleting),
        }
    }

    pub fn prepared_image(&self, commit_hash: &CommitHash) -> &PreparedImage {
        &self.prepared_image_map[commit_hash].1
    }

    pub fn graph(&self) -> &Graph<'a> {
        &self.graph
    }

    pub fn graph_cell_width(&self) -> u16 {
        let cell_count = (self.graph.max_pos_x + 1) as u16;
        match self.cell_width_type {
            CellWidthType::Double => cell_count * 2,
            CellWidthType::Single => cell_count,
        }
    }

    pub fn drain_pending_uploads(&mut self) -> Vec<String> {
        self.deleting.clear();
        std::mem::take(&mut self.pending_uploads)
    }

    // Drops the prepared images of the rows far from `shown` once there are too many. A row shown
    // again is prepared again under the same id, replacing its image in the terminal, as a row
    // drawn differently does; the terminal keeps the images, deleted with the manager.
    pub fn forget_far_from(&mut self, shown: Range<usize>) {
        if self.prepared_image_map.len() <= KEPT_IMAGES {
            return;
        }
        let near =
            shown.start.saturating_sub(KEPT_IMAGES / 4)..shown.end.saturating_add(KEPT_IMAGES / 4);
        let rows = &self.graph.commit_pos_map;
        self.prepared_image_map
            .retain(|hash, _| rows.get(hash).is_some_and(|(_, y)| near.contains(y)));
    }

    #[cfg(test)]
    pub fn prepared_count(&self) -> usize {
        self.prepared_image_map.len()
    }

    pub fn ensure_uploaded(&mut self, commit_hash: &CommitHash) {
        if self.prepared_image_map.contains_key(commit_hash) {
            return;
        }
        let graph_row_image = build_single_graph_row_image(
            &self.graph,
            &self.image_params,
            &mut self.drawing_pixels,
            self.graph_style,
            self.image_width_mode,
            commit_hash,
        );
        let drawn = fingerprint(&graph_row_image.bytes);
        let image = match self.previous.remove(commit_hash) {
            Some((previous, image)) if previous == drawn => image,
            // the same id, so a changed image replaces the one in the terminal
            _ => {
                let image_id = self.image_ids.of(commit_hash);
                let mut image =
                    graph_row_image.prepare(self.cell_width_type, self.image_protocol, image_id);
                if let Some(upload_data) = image.take_upload_data() {
                    self.pending_uploads.push(upload_data);
                }
                image
            }
        };
        self.prepared_image_map
            .insert(commit_hash.clone(), (drawn, image));
    }
}

// The prepared images kept, those of the rows far from the screen dropped beyond it: each holds its
// cells, a few kilobytes, and scrolling through 100k commits kept them all, hundreds of megabytes.
const KEPT_IMAGES: usize = 2000;

// What tells a row image from the one drawn before it of the same commit, kept in place of its
// PNG, a kilobyte a row: a hash of it, its length included.
fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    bytes.hash(&mut hasher);
    hasher.finish()
}

// The images a graph image manager left in the terminal, for the one drawing the graph laid out
// or loaded again, see `GraphImageManager::with_images`.
#[derive(Debug)]
pub struct GraphImages {
    images: FxHashMap<CommitHash, (u64, PreparedImage)>,
    image_ids: ImageIds,
    pending_uploads: Vec<String>,
    deleting: Vec<u32>,
}

// The manager owns the images it uploaded: deleting them when it is dropped keeps a page switch or
// a replaced submodule panel from leaving them in the terminal until the app quits.
impl Drop for GraphImageManager<'_> {
    fn drop(&mut self) {
        let mut ids = self.image_ids.all();
        ids.append(&mut self.deleting);
        let _ = self.image_protocol.delete_images(&ids);
    }
}

pub struct GraphRowImage {
    pub bytes: Vec<u8>,
    pub cell_count: usize,
}

impl Debug for GraphRowImage {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "GraphRowImage {{ bytes: [{} bytes], cell_count: {} }}",
            self.bytes.len(),
            self.cell_count
        )
    }
}

impl GraphRowImage {
    fn prepare(
        &self,
        cell_width_type: CellWidthType,
        image_protocol: ImageProtocol,
        image_id: u32,
    ) -> PreparedImage {
        let image_cell_width = match cell_width_type {
            CellWidthType::Double => self.cell_count * 2,
            CellWidthType::Single => self.cell_count,
        };
        image_protocol.prepare_image(&self.bytes, image_cell_width, image_id)
    }
}

fn create_session_nonce() -> u32 {
    let mut hasher = rustc_hash::FxHasher::default();
    process::id().hash(&mut hasher);
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    hasher.finish() as u32
}

#[derive(Debug)]
pub struct ImageParams {
    width: u16,
    height: u16,
    line_width: u16,
    circle_inner_radius: u16,
    circle_outer_radius: u16,
    // of a merge commit's node, see `Chevron`
    merge_radius: u16,
    edge_colors: Vec<image::Rgba<u8>>,
    circle_edge_color: image::Rgba<u8>,
    background_color: image::Rgba<u8>,
    antialias: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellWidthType {
    Double, // 2 cells
    Single,
}

impl ImageParams {
    pub fn new(
        graph_color_set: &GraphColorSet,
        cell_width_type: CellWidthType,
        antialias: bool,
    ) -> Self {
        let (width, height, line_width, circle_inner_radius, circle_outer_radius, merge_radius) =
            match cell_width_type {
                CellWidthType::Double => (50, 50, 5, 10, 13, 20),
                CellWidthType::Single => (25, 50, 3, 7, 10, 12),
            };
        let edge_colors = graph_color_set
            .colors
            .iter()
            .map(|c| c.to_image_color())
            .collect();
        let circle_edge_color = graph_color_set.edge_color.to_image_color();
        let background_color = graph_color_set.background_color.to_image_color();
        Self {
            width,
            height,
            line_width,
            circle_inner_radius,
            circle_outer_radius,
            merge_radius,
            edge_colors,
            circle_edge_color,
            background_color,
            antialias,
        }
    }

    // Makes a row image as tall, for its width, as the terminal's cells: (width, height) in pixels.
    // Terminals placing images by unicode placeholders (kitty in tmux) keep their proportions,
    // leaving gaps between the rows of a taller cell; the others stretch them to their cells.
    // shortcut: taken when the graph is laid out (at start, on each reload and for a submodule's),
    // changing the font size between brings the gaps back.
    pub fn fit_cell(mut self, cell_pixels: Option<(u16, u16)>) -> Self {
        if let Some((cell_width, cell_height)) = cell_pixels.filter(|&(w, h)| w > 0 && h > 0) {
            // a cell is 25 pixels wide in either width type
            let height =
                (25 * u32::from(cell_height) + u32::from(cell_width) / 2) / u32::from(cell_width);
            self.height = height.clamp(25, 125) as u16;
        }
        self
    }

    fn edge_color(&self, index: usize) -> image::Rgba<u8> {
        self.edge_colors[index % self.edge_colors.len()]
    }

    fn corner_radius(&self) -> u16 {
        if self.width < self.height {
            self.width / 2
        } else {
            self.height / 2
        }
    }
}

pub fn build_single_graph_row_image(
    graph: &Graph<'_>,
    image_params: &ImageParams,
    drawing_pixels: &mut DrawingPixels,
    graph_style: GraphStyle,
    image_width_mode: GraphImageWidthMode,
    commit_hash: &CommitHash,
) -> GraphRowImage {
    let (pos_x, pos_y) = graph.commit_pos_map[commit_hash];
    let edges = &graph.edges.at(pos_y);

    let max_pos_x = match image_width_mode {
        GraphImageWidthMode::Compact => edges.iter().map(|e| e.pos_x).fold(pos_x, usize::max),
        GraphImageWidthMode::Fixed => graph.max_pos_x,
    };

    let cell_count = max_pos_x + 1;

    calc_graph_row_image(
        pos_x,
        cell_count,
        edges,
        image_params,
        drawing_pixels,
        graph_style,
        graph.merges.get(commit_hash).copied(),
    )
}

// The node of a merge commit, larger than a commit's dot: a ring with a chevron in it, pointing to
// the side the merged line comes in from, or down once the merge is folded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Chevron {
    Left,
    Right,
    Down,
}

impl Chevron {
    const ALL: [Chevron; 3] = [Chevron::Left, Chevron::Right, Chevron::Down];

    // Of the merge commit at `pos_x` in the row of `edges`: a merged line turns down to its
    // parent (╮ or ╭) on the side it comes from; the right one first for an octopus merge.
    fn of(fold: Fold, edges: &[Edge], pos_x: usize) -> Self {
        let turns = |edge_type: EdgeType, side: fn(usize, usize) -> bool| {
            edges
                .iter()
                .any(|e| e.edge_type == edge_type && side(e.pos_x, pos_x))
        };
        match fold {
            Fold::Folded => Chevron::Down,
            _ if turns(EdgeType::RightTop, |x, pos_x| x > pos_x) => Chevron::Right,
            _ if turns(EdgeType::LeftTop, |x, pos_x| x < pos_x) => Chevron::Left,
            Fold::Unfolded => Chevron::Right,
        }
    }
}

// The center of a commit's node, a merge's or a circle.
fn merge_center(params: &ImageParams) -> Point {
    Point::new(
        f64::from(params.width / 2) + 0.5,
        f64::from(params.height / 2) + 0.5,
    )
}

// Whether `p` is in the node of a merge commit, cleared of the lines under it.
fn merge_disk_contains(p: Point, params: &ImageParams) -> bool {
    (p - merge_center(params)).length() <= f64::from(params.merge_radius) + 0.5
}

// Whether `p` is on the ring of a merge commit's node or on its chevron, see `Chevron`.
fn merge_mark_contains(p: Point, params: &ImageParams, chevron: Chevron) -> bool {
    let center = merge_center(params);
    let width = f64::from(params.line_width);
    let outer = f64::from(params.merge_radius) + 0.5;
    let inner = outer - width;
    let distance = (p - center).length();
    if inner < distance && distance <= outer {
        return true;
    }
    let at = |(x, y): (f64, f64)| center + Vector::new(x * inner, y * inner);
    let [start, tip, end] = match chevron {
        Chevron::Right => [(-0.25, -0.5), (0.3, 0.0), (-0.25, 0.5)],
        Chevron::Left => [(0.25, -0.5), (-0.3, 0.0), (0.25, 0.5)],
        Chevron::Down => [(-0.5, -0.25), (0.0, 0.3), (0.5, -0.25)],
    }
    .map(at);
    let near = |a: Point, b: Point| {
        let ab = b - a;
        let t = ((p - a).dot(ab) / ab.dot(ab)).clamp(0.0, 1.0);
        (p - (a + ab * t)).length() <= width / 2.0
    };
    near(start, tip) || near(tip, end)
}

type Pixels = FxHashSet<(i32, i32)>;

// For fixed ImageParams, colors and absolute columns do not affect the shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ConnectionKey {
    corner_type: EdgeType,
    distance: usize,
}

impl ConnectionKey {
    fn new(side: &Edge, corner: &Edge) -> Self {
        Self {
            corner_type: corner.edge_type,
            distance: side.pos_x.abs_diff(corner.pos_x),
        }
    }

    fn normalized_edges(self) -> (Edge, Edge) {
        let (side_type, side_x, corner_x) = match self.corner_type {
            EdgeType::RightTop | EdgeType::RightBottom => (EdgeType::Right, 0, self.distance),
            EdgeType::LeftTop | EdgeType::LeftBottom => (EdgeType::Left, self.distance, 0),
            _ => unreachable!(),
        };
        (
            Edge::new(side_type, side_x, 0),
            Edge::new(self.corner_type, corner_x, 0),
        )
    }
}

#[derive(Debug)]
pub struct DrawingPixels {
    antialias: Option<antialias::Masks>,
    diagonal_connections: FxHashMap<ConnectionKey, Pixels>,
    curved_connections: FxHashMap<ConnectionKey, Pixels>,
    circle: Pixels,
    circle_edge: Pixels,
    // the node of a merge commit instead of the circle, see `Chevron`
    merge_disk: Pixels,
    merge_marks: FxHashMap<Chevron, Pixels>,
    vertical_edge: Pixels,
    horizontal_edge: Pixels,
    up_edge: Pixels,
    down_edge: Pixels,
    left_edge: Pixels,
    right_edge: Pixels,
    right_top_edge: Pixels,
    left_top_edge: Pixels,
    right_bottom_edge: Pixels,
    left_bottom_edge: Pixels,
}

impl DrawingPixels {
    pub fn new(image_params: &ImageParams) -> Self {
        let (w, h) = (image_params.width as i32, image_params.height as i32);
        let (cx, cy) = (w / 2, h / 2);
        let r = image_params.circle_outer_radius as i32;
        let line_width = image_params.line_width as i32;
        // the columns of a vertical line and the rows of a horizontal one, through the center
        let lane_x = cx - line_width / 2..cx - line_width / 2 + line_width;
        let lane_y = cy - line_width / 2..cy - line_width / 2 + line_width;
        let inner = disk_pixels(cx, cy, image_params.circle_inner_radius as i32);
        let outer = disk_pixels(cx, cy, r);

        Self {
            diagonal_connections: FxHashMap::default(),
            curved_connections: FxHashMap::default(),
            antialias: image_params
                .antialias
                .then(|| antialias::Masks::new(image_params)),
            circle_edge: outer.difference(&inner).copied().collect(),
            circle: inner,
            merge_disk: shape_pixels(image_params, |p| merge_disk_contains(p, image_params)),
            merge_marks: Chevron::ALL
                .into_iter()
                .map(|chevron| {
                    let marks = shape_pixels(image_params, |p| {
                        merge_mark_contains(p, image_params, chevron)
                    });
                    (chevron, marks)
                })
                .collect(),
            vertical_edge: rect_pixels(lane_x.clone(), 0..h),
            horizontal_edge: rect_pixels(0..w, lane_y.clone()),
            up_edge: rect_pixels(lane_x.clone(), 0..cy - r),
            down_edge: rect_pixels(lane_x, cy + r + 1..h),
            left_edge: rect_pixels(0..cx - r, lane_y.clone()),
            right_edge: rect_pixels(cx + r + 1..w, lane_y),
            right_top_edge: corner_pixels(image_params, 0, h),
            left_top_edge: corner_pixels(image_params, w, h),
            right_bottom_edge: corner_pixels(image_params, 0, 0),
            left_bottom_edge: corner_pixels(image_params, w, 0),
        }
    }

    fn diagonal_connection_pixels(
        &mut self,
        side: &Edge,
        corner: &Edge,
        params: &ImageParams,
    ) -> &Pixels {
        let key = ConnectionKey::new(side, corner);
        self.diagonal_connections.entry(key).or_insert_with(|| {
            let (side, corner) = key.normalized_edges();
            calc_diagonal_connection_drawing_pixels(&side, &corner, params)
        })
    }

    fn curved_connection_pixels(
        &mut self,
        side: &Edge,
        corner: &Edge,
        params: &ImageParams,
    ) -> &Pixels {
        let key = ConnectionKey::new(side, corner);
        self.curved_connections.entry(key).or_insert_with(|| {
            let (side, corner) = key.normalized_edges();
            calc_curved_connection_drawing_pixels(&side, &corner, params)
        })
    }
}

// The pixels of a cell whose center is in a shape.
fn shape_pixels(image_params: &ImageParams, contains: impl Fn(Point) -> bool) -> Pixels {
    let (width, height) = (image_params.width as i32, image_params.height as i32);
    (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .filter(|&(x, y)| contains(Point::new(f64::from(x) + 0.5, f64::from(y) + 0.5)))
        .collect()
}

// The pixels of a disk of `radius` around (`cx`, `cy`), by Bresenham's circle algorithm.
fn disk_pixels(cx: i32, cy: i32, radius: i32) -> Pixels {
    let mut x = radius;
    let mut y = 0;
    let mut p = 1 - radius;

    let mut pixels = Pixels::default();

    while x >= y {
        for dx in -x..=x {
            pixels.insert((cx + dx, cy + y));
            pixels.insert((cx + dx, cy - y));
        }
        for dx in -y..=y {
            pixels.insert((cx + dx, cy + x));
            pixels.insert((cx + dx, cy - x));
        }

        y += 1;
        if p <= 0 {
            p += 2 * y + 1;
        } else {
            x -= 1;
            p += 2 * y - 2 * x + 1;
        }
    }

    pixels
}

fn rect_pixels(xs: Range<i32>, ys: Range<i32>) -> Pixels {
    ys.flat_map(|y| xs.clone().map(move |x| (x, y))).collect()
}

// The rounded corner from the center of the cell to the middle of its side at `base_x`
// (0 or the width) or of its top or bottom at `base_y` (the height or 0): a quarter ring around
// that corner of the cell, moved to meet the line in the cell's longer direction, which a straight
// part then joins to the center.
fn corner_pixels(image_params: &ImageParams, base_x: i32, base_y: i32) -> Pixels {
    let (w, h) = (image_params.width as i32, image_params.height as i32);
    let r = image_params.corner_radius() as i32;
    let (x_offset, y_offset) = match (w < h, base_x == 0, base_y == 0) {
        (true, _, true) => (0, h / 2 - r),
        (true, _, false) => (0, r - h / 2),
        (false, true, _) => (w / 2 - r, 0),
        (false, false, _) => (r - w / 2, 0),
    };
    let line_width = image_params.line_width as i32;
    let half_line_width = line_width / 2;
    let adjust = if image_params.line_width.is_multiple_of(2) {
        0
    } else {
        1
    };
    let inner = disk_pixels(base_x, base_y, r - half_line_width - adjust);
    let outer = disk_pixels(base_x, base_y, r + half_line_width);

    let mut pixels: Pixels = outer
        .difference(&inner)
        .filter(|p| p.0 >= 0 && p.0 < w && p.1 >= 0 && p.1 < h)
        .map(|p| (p.0 + x_offset, p.1 + y_offset))
        .collect();

    if w < h {
        let ys = match y_offset < 0 {
            true => base_y + y_offset..base_y,
            false => base_y..base_y + y_offset,
        };
        let x_start = w / 2 - line_width / 2;
        pixels.extend(rect_pixels(x_start..x_start + line_width, ys));
    }
    if w > h {
        let xs = match x_offset < 0 {
            true => base_x + x_offset..base_x,
            false => base_x..base_x + x_offset,
        };
        let y_start = h / 2 - line_width / 2;
        pixels.extend(rect_pixels(xs, y_start..y_start + line_width));
    }

    pixels
}

pub fn calc_graph_row_image(
    commit_pos_x: usize,
    cell_count: usize,
    edges: &[Edge],
    image_params: &ImageParams,
    drawing_pixels: &mut DrawingPixels,
    graph_style: GraphStyle,
    merge: Option<Fold>,
) -> GraphRowImage {
    let image_width = (image_params.width as usize * cell_count) as u32;
    let image_height = image_params.height as u32;
    let chevron = merge.map(|fold| Chevron::of(fold, edges, commit_pos_x));

    // Paint whole lanes from left to right, so higher lane indices cover lower ones.
    let draw_order_key = |e: &Edge| (e.associated_line_pos_x, e.pos_x, e.edge_type);
    let ordered_edges = if edges.is_sorted_by_key(draw_order_key) {
        Cow::Borrowed(edges)
    } else {
        let mut ordered_edges = edges.to_vec();
        ordered_edges.sort_unstable_by_key(draw_order_key);
        Cow::Owned(ordered_edges)
    };
    let edges = ordered_edges.as_ref();

    if image_params.antialias {
        let image = antialias::render(
            commit_pos_x,
            cell_count,
            edges,
            image_params,
            drawing_pixels,
            graph_style,
            chevron,
        );
        return GraphRowImage {
            bytes: build_image(&image, image_width, image_height),
            cell_count,
        };
    }

    let mut img_buf = image::ImageBuffer::new(image_width, image_height);

    draw_background(&mut img_buf, image_params);
    if chevron.is_none() {
        draw_commit_circle(&mut img_buf, commit_pos_x, image_params, drawing_pixels);
    }

    for lane_edges in edges.chunk_by(|a, b| a.associated_line_pos_x == b.associated_line_pos_x) {
        for edge in lane_edges {
            if graph_style == GraphStyle::Rounded || edge.edge_type.is_vertically_related() {
                draw_edge(&mut img_buf, edge, image_params, drawing_pixels);
            }
        }
        if graph_style != GraphStyle::Rounded {
            draw_connected_edges(
                &mut img_buf,
                lane_edges,
                image_params,
                drawing_pixels,
                graph_style,
            );
        }
    }

    // over the lines, which it hides under it
    if let Some(chevron) = chevron {
        let x_offset = (commit_pos_x * image_params.width as usize) as i32;
        let color = image_params.edge_color(commit_pos_x);
        for (pixels, color) in [
            (&drawing_pixels.merge_disk, image_params.background_color),
            (&drawing_pixels.merge_marks[&chevron], color),
        ] {
            draw_pixels(&mut img_buf, pixels, x_offset, color);
        }
    }

    let bytes = build_image(&img_buf, image_width, image_height);

    GraphRowImage { bytes, cell_count }
}

fn draw_background(
    img_buf: &mut image::ImageBuffer<image::Rgba<u8>, Vec<u8>>,
    image_params: &ImageParams,
) {
    if image_params.background_color[3] == 0 {
        // If the alpha value is 0, the background is transparent, so we don't need to draw it.
        return;
    }
    for pixel in img_buf.pixels_mut() {
        *pixel = image_params.background_color;
    }
}

fn draw_commit_circle(
    img_buf: &mut image::RgbaImage,
    circle_pos_x: usize,
    image_params: &ImageParams,
    drawing_pixels: &DrawingPixels,
) {
    let x_offset = (circle_pos_x * image_params.width as usize) as i32;
    let color = image_params.edge_color(circle_pos_x);
    draw_pixels(img_buf, &drawing_pixels.circle, x_offset, color);
    // a transparent edge isn't drawn
    if image_params.circle_edge_color[3] != 0 {
        let color = image_params.circle_edge_color;
        draw_pixels(img_buf, &drawing_pixels.circle_edge, x_offset, color);
    }
}

fn draw_edge(
    img_buf: &mut image::RgbaImage,
    edge: &Edge,
    image_params: &ImageParams,
    drawing_pixels: &DrawingPixels,
) {
    let pixels = match edge.edge_type {
        EdgeType::Vertical => &drawing_pixels.vertical_edge,
        EdgeType::Horizontal => &drawing_pixels.horizontal_edge,
        EdgeType::Up => &drawing_pixels.up_edge,
        EdgeType::Down => &drawing_pixels.down_edge,
        EdgeType::Left => &drawing_pixels.left_edge,
        EdgeType::Right => &drawing_pixels.right_edge,
        EdgeType::RightTop => &drawing_pixels.right_top_edge,
        EdgeType::RightBottom => &drawing_pixels.right_bottom_edge,
        EdgeType::LeftTop => &drawing_pixels.left_top_edge,
        EdgeType::LeftBottom => &drawing_pixels.left_bottom_edge,
    };
    let x_offset = (edge.pos_x * image_params.width as usize) as i32;
    let color = image_params.edge_color(edge.associated_line_pos_x);
    draw_pixels(img_buf, pixels, x_offset, color);
}

fn edge_connections(edges: &[Edge]) -> impl Iterator<Item = (&Edge, &Edge)> {
    edges.iter().filter_map(|corner| {
        let side_type = match corner.edge_type {
            EdgeType::RightBottom | EdgeType::RightTop => EdgeType::Right,
            EdgeType::LeftBottom | EdgeType::LeftTop => EdgeType::Left,
            _ => return None,
        };
        let side = edges.iter().find(|e| {
            e.edge_type == side_type && e.associated_line_pos_x == corner.associated_line_pos_x
        })?;
        Some((side, corner))
    })
}

fn draw_connected_edges(
    img_buf: &mut image::ImageBuffer<image::Rgba<u8>, Vec<u8>>,
    edges: &[Edge],
    image_params: &ImageParams,
    drawing_pixels: &mut DrawingPixels,
    graph_style: GraphStyle,
) {
    for (side, corner) in edge_connections(edges) {
        let color = image_params.edge_color(side.associated_line_pos_x);
        let x_offset = (side.pos_x.min(corner.pos_x) * image_params.width as usize) as i32;
        let pixels = match graph_style {
            GraphStyle::Angular => {
                drawing_pixels.diagonal_connection_pixels(side, corner, image_params)
            }
            GraphStyle::Curved => {
                drawing_pixels.curved_connection_pixels(side, corner, image_params)
            }
            GraphStyle::Rounded => unreachable!(),
        };
        draw_pixels(img_buf, pixels, x_offset, color);
    }
}

// Draws `pixels` moved right by `x_offset`, those past the image left out: a circle is taller than
// the shortest rows, of square cells.
fn draw_pixels(
    img_buf: &mut image::RgbaImage,
    pixels: &Pixels,
    x_offset: i32,
    color: image::Rgba<u8>,
) {
    for &(x, y) in pixels {
        let x = x + x_offset;
        if x >= 0 && y >= 0 && x < img_buf.width() as i32 && y < img_buf.height() as i32 {
            *img_buf.get_pixel_mut(x as u32, y as u32) = color;
        }
    }
}

fn calc_diagonal_connection_drawing_pixels(
    side: &Edge,
    corner: &Edge,
    params: &ImageParams,
) -> Pixels {
    let (vertices, ys, x_start) = diagonal_connection(side, corner, params);
    let (min_x, min_y, max_x, max_y) = bounding_box_u32(&vertices);
    let mut pixels = Pixels::default();
    for y in min_y..max_y.min(u32::from(params.height)) {
        for x in min_x..max_x {
            let p = Point::new(x as f64 + 0.5, y as f64 + 0.5);
            if p.is_inside_polygon(&vertices) {
                pixels.insert((x as i32, y as i32));
            }
        }
    }
    for y in ys {
        for x in x_start..x_start + i32::from(params.line_width) {
            pixels.insert((x, y as i32));
        }
    }
    pixels
}

fn diagonal_connection(
    side: &Edge,
    corner: &Edge,
    image_params: &ImageParams,
) -> ([Point; 4], std::ops::Range<u32>, i32) {
    let half_width = f64::from(image_params.line_width) / 2.0;
    // Keep the existing diagonal-to-vertical junction for each cell aspect ratio.
    let y_offset = if image_params.width == image_params.height {
        f64::from(image_params.height) / 10.0
    } else {
        f64::from(image_params.height) / 2.0 - f64::from(image_params.corner_radius())
    };
    let turns_up = matches!(
        corner.edge_type,
        EdgeType::RightBottom | EdgeType::LeftBottom
    );
    let start = Point::new(
        (side.pos_x * image_params.width as usize) as f64 + f64::from(image_params.width) / 2.0,
        f64::from(image_params.height) / 2.0,
    );
    let end = Point::new(
        (corner.pos_x * image_params.width as usize) as f64 + f64::from(image_params.width) / 2.0,
        if turns_up {
            y_offset
        } else {
            f64::from(image_params.height) - y_offset
        },
    );
    let unit = (end - start).normalize();
    let normal = unit.perpendicular();
    let line_start = start + unit * f64::from(image_params.circle_outer_radius);
    let line_start_1 = line_start + normal * half_width;
    let line_start_2 = line_start - normal * half_width;
    let slope = unit.y / unit.x;
    let (x1, x2) = if turns_up {
        (end.x + half_width, end.x - half_width)
    } else {
        (end.x - half_width, end.x + half_width)
    };
    let corner_1 = Point::new(x1, line_start_1.y + slope * (x1 - line_start_1.x));
    let corner_2 = Point::new(x2, line_start_2.y + slope * (x2 - line_start_2.x));
    let ys = if turns_up {
        0..corner_1.y.max(corner_2.y) as u32
    } else {
        (corner_1.y.min(corner_2.y) as u32 + 1)..u32::from(image_params.height)
    };
    let x_start = end.x as i32 - i32::from(image_params.line_width) / 2;
    (
        [line_start_1, corner_1, corner_2, line_start_2],
        ys,
        x_start,
    )
}

fn curved_connection_points(side: &Edge, corner: &Edge, image_params: &ImageParams) -> [Point; 4] {
    let direction = if side.edge_type == EdgeType::Right {
        1.0
    } else {
        -1.0
    };
    // Align stroke centers with the existing straight-edge pixel masks.
    let pixel_offset = f64::from(image_params.line_width % 2) / 2.0;
    let center_y = f64::from(image_params.height / 2) + pixel_offset;
    let start_x = (side.pos_x * image_params.width as usize) as f64
        + f64::from(image_params.width / 2)
        + 0.5
        + direction * (f64::from(image_params.circle_outer_radius) + 1.0);
    let end_x = (corner.pos_x * image_params.width as usize) as f64
        + f64::from(image_params.width / 2)
        + pixel_offset;
    let end_y = match corner.edge_type {
        EdgeType::RightBottom | EdgeType::LeftBottom => 0.0,
        _ => f64::from(image_params.height),
    };

    // Leave the circle horizontally and meet the row boundary vertically.
    // Control points stay within the connection's existing row and columns.
    [
        Point::new(start_x, center_y),
        Point::new(start_x + (end_x - start_x) * 0.60, center_y),
        Point::new(end_x, end_y + (center_y - end_y) * 0.65),
        Point::new(end_x, end_y),
    ]
}

fn bezier_point(points: &[Point; 4], t: f64) -> Point {
    let u = 1.0 - t;
    Point::new(
        u * u * u * points[0].x
            + 3.0 * u * u * t * points[1].x
            + 3.0 * u * t * t * points[2].x
            + t * t * t * points[3].x,
        u * u * u * points[0].y
            + 3.0 * u * u * t * points[1].y
            + 3.0 * u * t * t * points[2].y
            + t * t * t * points[3].y,
    )
}

// The curve of a connection cut into short straight pieces, (start, end), enough of them for
// the strokes drawn along them to look smooth, see `near_piece`.
fn curve_pieces(points: [Point; 4]) -> impl Iterator<Item = (Point, Point)> {
    let length = (points[3].x - points[0].x).abs() + (points[3].y - points[0].y).abs();
    let steps = ((length * 3.0).ceil() as usize).max(32);
    (1..=steps).scan(points[0], move |start, step| {
        let end = bezier_point(&points, step as f64 / steps as f64);
        Some((std::mem::replace(start, end), end))
    })
}

// Whether `p` is within `radius` of the piece of a curve from `start` to `end`.
fn near_piece(p: Point, (start, end): (Point, Point), radius: f64) -> bool {
    let segment = end - start;
    let length_squared = segment.dot(segment);
    let t = if length_squared == 0.0 {
        0.0
    } else {
        ((p - start).dot(segment) / length_squared).clamp(0.0, 1.0)
    };
    let delta = p - (start + segment * t);
    delta.dot(delta) <= radius * radius
}

fn calc_curved_connection_drawing_pixels(
    side: &Edge,
    corner: &Edge,
    params: &ImageParams,
) -> Pixels {
    let points = curved_connection_points(side, corner, params);
    let radius = f64::from(params.line_width) / 2.0;
    let mut pixels = Pixels::default();
    let direction = (points[3].x - points[0].x).signum();
    let boundary_y = if points[3].y == 0.0 {
        0
    } else {
        u32::from(params.height) - 1
    };
    // the pixels whose center is within the stroke along each piece
    for piece @ (start, end) in curve_pieces(points) {
        let bounds = [
            Point::new(
                start.x.min(end.x) - radius - 1.0,
                start.y.min(end.y) - radius - 1.0,
            ),
            Point::new(
                start.x.max(end.x) + radius + 1.0,
                start.y.max(end.y) + radius + 1.0,
            ),
        ];
        let (min_x, min_y, max_x, max_y) = bounding_box_u32(&bounds);
        for y in min_y..max_y.min(u32::from(params.height)) {
            if y == boundary_y {
                continue;
            }
            for x in min_x..max_x {
                let pixel_center = Point::new(x as f64 + 0.5, y as f64 + 0.5);
                // Keep the same circle-side endpoint, with a flat stroke cap.
                if (pixel_center.x - points[0].x) * direction >= 0.0
                    && near_piece(pixel_center, piece, radius)
                {
                    pixels.insert((x as i32, y as i32));
                }
            }
        }
    }

    // Match the existing vertical mask exactly at the row boundary.
    let x_start = (points[3].x - radius) as i32;
    for x in x_start..x_start + i32::from(params.line_width) {
        pixels.insert((x, boundary_y as i32));
    }
    pixels
}

fn build_image(img_buf: &[u8], image_width: u32, image_height: u32) -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image::write_buffer_with_format(
        &mut bytes,
        img_buf,
        image_width,
        image_height,
        image::ColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .unwrap();
    bytes.into_inner()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use image::GenericImage;
    use rstest::rstest;
    use EdgeType::*;

    use super::*;
    use crate::{config::GraphColorConfig, test_support::image_snapshot::compare_image_snapshot};

    #[test]
    fn test_image_ids_are_each_commits_own() {
        let mut ids = super::ImageIds::new();
        let hash = |i: u128| {
            CommitHash::from(
                format!(
                    "{:040x}",
                    i.wrapping_mul(0x9E37_79B9_7F4A_7C15_F39C_C060_5CED_C835)
                )
                .as_str(),
            )
        };
        // as many commits as a hash of each gave two the same id
        let given: rustc_hash::FxHashSet<u32> = (0..200_000).map(|i| ids.of(&hash(i))).collect();
        assert_eq!(given.len(), 200_000);
        assert!(!given.contains(&0));
        // the same one while the image is there, a new one once it was deleted
        let first = ids.of(&hash(0));
        assert_eq!(ids.of(&hash(0)), first);
        assert_eq!(ids.remove(&hash(0)), Some(first));
        assert_ne!(ids.of(&hash(0)), first);
    }

    #[test]
    fn test_fit_cell() {
        let params = |cell| {
            let colors = GraphColorSet::new(&crate::config::GraphColorConfig::default());
            ImageParams::new(&colors, CellWidthType::Double, false).fit_cell(cell)
        };
        // as tall as the cells: 9x23 pixels, a line height of 115%
        assert_eq!(
            (params(Some((9, 23))).width, params(Some((9, 23))).height),
            (50, 64)
        );
        // unknown, the 1:2 cells assumed
        assert_eq!(params(None).height, 50);
        assert_eq!(params(Some((0, 0))).height, 50);
    }

    // The shortest rows, of square cells, drew a commit's circle past the row and panicked, with
    // its edge drawn.
    #[test]
    fn test_every_row_height_draws() {
        let colors = GraphColorSet::new(&crate::config::GraphColorConfig {
            edge: "#ffffff".into(),
            ..Default::default()
        });
        let edges: Vec<Edge> = [
            EdgeType::Up,
            EdgeType::Down,
            EdgeType::Vertical,
            EdgeType::RightTop,
            EdgeType::LeftBottom,
        ]
        .into_iter()
        .map(|t| Edge::new(t, 1, 1))
        .collect();
        for cell_width_type in [CellWidthType::Double, CellWidthType::Single] {
            for antialias in [false, true] {
                for cell in [(10, 10), (10, 13), (9, 23), (5, 30)] {
                    let params =
                        ImageParams::new(&colors, cell_width_type, antialias).fit_cell(Some(cell));
                    let mut pixels = DrawingPixels::new(&params);
                    for style in [GraphStyle::Rounded, GraphStyle::Angular, GraphStyle::Curved] {
                        for merge in [None, Some(Fold::Unfolded)] {
                            calc_graph_row_image(1, 3, &edges, &params, &mut pixels, style, merge);
                        }
                    }
                }
            }
        }
    }

    const OUTPUT_DIR: &str = "./out/ut/graph/image";
    const SNAPSHOT_DIR: &str = "./tests/graph/image";

    type TestParam = (usize, Vec<(EdgeType, usize, usize)>);

    #[rstest]
    #[case("default_params_rounded", GraphStyle::Rounded)]
    #[case("default_params_angular", GraphStyle::Angular)]
    #[case("default_params_curved", GraphStyle::Curved)]
    fn test_calc_graph_row_image_default_params(
        #[case] file_name: &str,
        #[case] graph_style: GraphStyle,
        #[values(false, true)] antialias: bool,
    ) {
        let params = simple_test_params();
        let cell_count = 4;
        let graph_color_config = GraphColorConfig::default();
        let graph_color_set = GraphColorSet::new(&graph_color_config);
        let cell_width_type = CellWidthType::Double;
        let image_params = ImageParams::new(&graph_color_set, cell_width_type, antialias);
        let drawing_pixels = DrawingPixels::new(&image_params);

        test_calc_graph_row_image(
            params,
            cell_count,
            image_params,
            drawing_pixels,
            graph_style,
            file_name,
        );
    }

    #[rstest]
    #[case("wide_image_rounded", GraphStyle::Rounded)]
    #[case("wide_image_angular", GraphStyle::Angular)]
    #[case("wide_image_curved", GraphStyle::Curved)]
    fn test_calc_graph_row_image_wide_image(
        #[case] file_name: &str,
        #[case] graph_style: GraphStyle,
        #[values(false, true)] antialias: bool,
    ) {
        let params = simple_test_params();
        let cell_count = 4;
        let graph_color_config = GraphColorConfig::default();
        let graph_color_set = GraphColorSet::new(&graph_color_config);
        let cell_width_type = CellWidthType::Double;
        let mut image_params = ImageParams::new(&graph_color_set, cell_width_type, antialias);
        image_params.width = 100;
        let drawing_pixels = DrawingPixels::new(&image_params);

        test_calc_graph_row_image(
            params,
            cell_count,
            image_params,
            drawing_pixels,
            graph_style,
            file_name,
        );
    }

    #[rstest]
    #[case("tall_image_rounded", GraphStyle::Rounded)]
    #[case("tall_image_angular", GraphStyle::Angular)]
    #[case("tall_image_curved", GraphStyle::Curved)]
    fn test_calc_graph_row_image_tall_image(
        #[case] file_name: &str,
        #[case] graph_style: GraphStyle,
        #[values(false, true)] antialias: bool,
    ) {
        let params = simple_test_params();
        let cell_count = 4;
        let graph_color_config = GraphColorConfig::default();
        let graph_color_set = GraphColorSet::new(&graph_color_config);
        let cell_width_type = CellWidthType::Double;
        let mut image_params = ImageParams::new(&graph_color_set, cell_width_type, antialias);
        image_params.height = 100;
        let drawing_pixels = DrawingPixels::new(&image_params);

        test_calc_graph_row_image(
            params,
            cell_count,
            image_params,
            drawing_pixels,
            graph_style,
            file_name,
        );
    }

    #[rstest]
    #[case("single_cell_width_rounded", GraphStyle::Rounded)]
    #[case("single_cell_width_angular", GraphStyle::Angular)]
    #[case("single_cell_width_curved", GraphStyle::Curved)]
    fn test_calc_graph_row_image_single_cell_width(
        #[case] file_name: &str,
        #[case] graph_style: GraphStyle,
        #[values(false, true)] antialias: bool,
    ) {
        let params = simple_test_params();
        let cell_count = 4;
        let graph_color_config = GraphColorConfig::default();
        let graph_color_set = GraphColorSet::new(&graph_color_config);
        let cell_width_type = CellWidthType::Single;
        let image_params = ImageParams::new(&graph_color_set, cell_width_type, antialias);
        let drawing_pixels = DrawingPixels::new(&image_params);

        test_calc_graph_row_image(
            params,
            cell_count,
            image_params,
            drawing_pixels,
            graph_style,
            file_name,
        );
    }

    #[rstest]
    #[case("circle_radius_rounded", GraphStyle::Rounded)]
    #[case("circle_radius_angular", GraphStyle::Angular)]
    #[case("circle_radius_curved", GraphStyle::Curved)]
    fn test_calc_graph_row_image_circle_radius(
        #[case] file_name: &str,
        #[case] graph_style: GraphStyle,
        #[values(false, true)] antialias: bool,
    ) {
        let params = straight_test_params();
        let cell_count = 2;
        let graph_color_config = GraphColorConfig::default();
        let graph_color_set = GraphColorSet::new(&graph_color_config);
        let cell_width_type = CellWidthType::Double;
        let mut image_params = ImageParams::new(&graph_color_set, cell_width_type, antialias);
        image_params.circle_inner_radius = 5;
        image_params.circle_outer_radius = 12;
        let drawing_pixels = DrawingPixels::new(&image_params);

        test_calc_graph_row_image(
            params,
            cell_count,
            image_params,
            drawing_pixels,
            graph_style,
            file_name,
        );
    }

    #[rstest]
    #[case("line_width_rounded", GraphStyle::Rounded)]
    #[case("line_width_angular", GraphStyle::Angular)]
    #[case("line_width_curved", GraphStyle::Curved)]
    fn test_calc_graph_row_image_line_width(
        #[case] file_name: &str,
        #[case] graph_style: GraphStyle,
        #[values(false, true)] antialias: bool,
    ) {
        let params = straight_test_params();
        let cell_count = 2;
        let graph_color_config = GraphColorConfig::default();
        let graph_color_set = GraphColorSet::new(&graph_color_config);
        let cell_width_type = CellWidthType::Double;
        let mut image_params = ImageParams::new(&graph_color_set, cell_width_type, antialias);
        image_params.line_width = 1;
        let drawing_pixels = DrawingPixels::new(&image_params);

        test_calc_graph_row_image(
            params,
            cell_count,
            image_params,
            drawing_pixels,
            graph_style,
            file_name,
        );
    }

    #[rstest]
    #[case("color_rounded", GraphStyle::Rounded)]
    #[case("color_angular", GraphStyle::Angular)]
    #[case("color_curved", GraphStyle::Curved)]
    fn test_calc_graph_row_image_color(
        #[case] file_name: &str,
        #[case] graph_style: GraphStyle,
        #[values(false, true)] antialias: bool,
    ) {
        let params = branches_test_params();
        let cell_count = 7;
        let graph_color_config = GraphColorConfig {
            branches: vec![
                "#c8c864".into(),
                "#64c8c8".into(),
                "#646464".into(),
                "#c864c8".into(),
            ],
            edge: "#ffffff".into(),
            background: "#00ff0070".into(),
        };
        let graph_color_set = GraphColorSet::new(&graph_color_config);
        let cell_width_type = CellWidthType::Double;
        let image_params = ImageParams::new(&graph_color_set, cell_width_type, antialias);
        let drawing_pixels = DrawingPixels::new(&image_params);

        test_calc_graph_row_image(
            params,
            cell_count,
            image_params,
            drawing_pixels,
            graph_style,
            file_name,
        );
    }

    #[rstest]
    fn test_lane_paint_order(
        #[values(GraphStyle::Rounded, GraphStyle::Angular, GraphStyle::Curved)] style: GraphStyle,
        #[values(CellWidthType::Double, CellWidthType::Single)] cell_width_type: CellWidthType,
        #[values(false, true)] antialias: bool,
    ) {
        let colors = GraphColorSet::new(&GraphColorConfig::default());
        let params = ImageParams::new(&colors, cell_width_type, antialias);
        let mut pixels = DrawingPixels::new(&params);
        let render = |edges: &[Edge], pixels: &mut DrawingPixels| {
            calc_graph_row_image(5, 6, edges, &params, pixels, style, None)
        };
        let left_connection = vec![
            Edge::new(LeftTop, 0, 0),
            Edge::new(Horizontal, 1, 0),
            Edge::new(Horizontal, 2, 0),
            Edge::new(Horizontal, 3, 0),
            Edge::new(Left, 4, 0),
        ];
        let vertical = vec![Edge::new(Vertical, 1, 1)];
        let right_connection = vec![
            Edge::new(Right, 0, 4),
            Edge::new(Horizontal, 1, 4),
            Edge::new(Horizontal, 2, 4),
            Edge::new(Horizontal, 3, 4),
            Edge::new(RightTop, 4, 4),
        ];
        let short_connection = vec![
            Edge::new(Right, 0, 3),
            Edge::new(Horizontal, 1, 3),
            Edge::new(Horizontal, 2, 3),
            Edge::new(RightTop, 3, 3),
        ];

        for (lower, higher) in [
            (left_connection, vertical.clone()),
            (vertical, right_connection.clone()),
            (short_connection, right_connection),
        ] {
            let lower_image = image::load_from_memory(&render(&lower, &mut pixels).bytes)
                .unwrap()
                .to_rgba8();
            let higher_image = image::load_from_memory(&render(&higher, &mut pixels).bytes)
                .unwrap()
                .to_rgba8();
            let lower_color = params.edge_color(lower[0].associated_line_pos_x);
            let higher_color = params.edge_color(higher[0].associated_line_pos_x);
            let mut edges = lower;
            edges.extend(higher);
            let combined = render(&edges, &mut pixels);
            let combined_image = image::load_from_memory(&combined.bytes).unwrap().to_rgba8();
            let mut overlap_count = 0;
            for (x, y, pixel) in lower_image.enumerate_pixels() {
                if *pixel == lower_color && *higher_image.get_pixel(x, y) == higher_color {
                    assert_eq!(
                        *combined_image.get_pixel(x, y),
                        higher_color,
                        "higher lane must cover lower lane at ({x}, {y})"
                    );
                    overlap_count += 1;
                }
            }
            assert!(overlap_count > 0, "test lanes must overlap");

            edges.reverse();
            assert_eq!(combined.bytes, render(&edges, &mut pixels).bytes);
            edges.rotate_left(1);
            assert_eq!(combined.bytes, render(&edges, &mut pixels).bytes);
        }
    }

    #[rstest]
    #[case(RightTop)]
    #[case(RightBottom)]
    #[case(LeftTop)]
    #[case(LeftBottom)]
    fn test_connection_cache(
        #[case] corner_type: EdgeType,
        #[values(CellWidthType::Double, CellWidthType::Single)] cell_width_type: CellWidthType,
        #[values(1, 2, 6)] distance: usize,
        #[values(GraphStyle::Angular, GraphStyle::Curved)] style: GraphStyle,
        #[values(false, true)] antialias: bool,
    ) {
        let colors = GraphColorSet::new(&GraphColorConfig {
            branches: vec![
                "#c8646480".into(),
                "#64c864c0".into(),
                "#6464c840".into(),
                "#c8c864ff".into(),
            ],
            edge: "#ffffff90".into(),
            background: "#00ff0070".into(),
        });
        let params = ImageParams::new(&colors, cell_width_type, antialias);
        let mut pixels = DrawingPixels::new(&params);
        assert!(pixels.diagonal_connections.is_empty());
        assert!(pixels.curved_connections.is_empty());

        for offset in [0, 3, 1] {
            let (side_type, side_x, corner_x) = match corner_type {
                RightTop | RightBottom => (Right, offset, offset + distance),
                _ => (Left, offset + distance, offset),
            };
            let edges = [
                Edge::new(side_type, side_x, offset + 1),
                Edge::new(corner_type, corner_x, offset + 1),
            ];
            // Reuse the shape across colors, placements and image widths,
            // including after a first rendering that clips the connection.
            for cell_count in [side_x + 1, offset + distance + 1, offset + distance + 4] {
                let cached = calc_graph_row_image(
                    side_x,
                    cell_count,
                    &edges,
                    &params,
                    &mut pixels,
                    style,
                    None,
                );
                let fresh = calc_graph_row_image(
                    side_x,
                    cell_count,
                    &edges,
                    &params,
                    &mut DrawingPixels::new(&params),
                    style,
                    None,
                );
                assert_eq!(cached.bytes, fresh.bytes);
                let cache = match style {
                    GraphStyle::Angular => &pixels.diagonal_connections,
                    GraphStyle::Curved => &pixels.curved_connections,
                    GraphStyle::Rounded => unreachable!(),
                };
                assert_eq!(cache.len(), usize::from(!antialias));
            }
        }
    }

    #[rstest]
    #[case(CellWidthType::Double)]
    #[case(CellWidthType::Single)]
    fn test_curved_connection_positions(#[case] cell_width_type: CellWidthType) {
        let colors = GraphColorSet::new(&GraphColorConfig::default());
        let params = ImageParams::new(&colors, cell_width_type, false);
        let mut pixels = DrawingPixels::new(&params);

        for (commit_pos_x, edges) in simple_test_params() {
            let edges: Vec<Edge> = edges
                .into_iter()
                .map(|(t, x, line)| Edge::new(t, x, line))
                .collect();
            let rounded = calc_graph_row_image(
                commit_pos_x,
                4,
                &edges,
                &params,
                &mut pixels,
                GraphStyle::Rounded,
                None,
            );
            let curved = calc_graph_row_image(
                commit_pos_x,
                4,
                &edges,
                &params,
                &mut pixels,
                GraphStyle::Curved,
                None,
            );
            assert_eq!(rounded.cell_count, curved.cell_count);
            let rounded = image::load_from_memory(&rounded.bytes).unwrap().to_rgba8();
            let curved = image::load_from_memory(&curved.bytes).unwrap().to_rgba8();
            assert_eq!(rounded.dimensions(), curved.dimensions());
            assert!(curved.pixels().all(|pixel| matches!(pixel[3], 0 | 255)));

            for y in [0, rounded.height() - 1] {
                for x in 0..rounded.width() {
                    assert_eq!(rounded.get_pixel(x, y), curved.get_pixel(x, y));
                }
            }
            let x_offset = (commit_pos_x * params.width as usize) as u32;
            for &(x, y) in pixels.circle.union(&pixels.circle_edge) {
                let (x, y) = (x as u32 + x_offset, y as u32);
                assert_eq!(rounded.get_pixel(x, y), curved.get_pixel(x, y));
            }
            // Circle-side endpoints stay at the existing left/right positions.
            for edge in edges.iter().filter(|e| matches!(e.edge_type, Left | Right)) {
                let center_x =
                    (edge.pos_x * params.width as usize) as u32 + u32::from(params.width / 2);
                let offset = u32::from(params.circle_outer_radius) + 1;
                let x = if edge.edge_type == Right {
                    center_x + offset
                } else {
                    center_x - offset
                };
                let y = u32::from(params.height / 2);
                assert_eq!(rounded.get_pixel(x, y), curved.get_pixel(x, y));
            }
        }
    }

    #[rstest]
    #[case(GraphStyle::Rounded)]
    #[case(GraphStyle::Angular)]
    #[case(GraphStyle::Curved)]
    fn test_antialias_connection_boundaries(
        #[case] style: GraphStyle,
        #[values(CellWidthType::Double, CellWidthType::Single)] cell_width_type: CellWidthType,
    ) {
        let colors = GraphColorSet::new(&GraphColorConfig::default());
        let params = ImageParams::new(&colors, cell_width_type, false);
        let mut pixels = DrawingPixels::new(&params);
        let aa_params = ImageParams::new(&colors, cell_width_type, true);
        let mut aa_pixels = DrawingPixels::new(&aa_params);
        for (pos, edges) in simple_test_params() {
            let edges: Vec<_> = edges
                .into_iter()
                .map(|(t, x, line)| Edge::new(t, x, line))
                .collect();
            let aliased = calc_graph_row_image(pos, 4, &edges, &params, &mut pixels, style, None);
            let aa = calc_graph_row_image(pos, 4, &edges, &aa_params, &mut aa_pixels, style, None);
            assert_eq!(aliased.cell_count, aa.cell_count);
            let aliased = image::load_from_memory(&aliased.bytes).unwrap().to_rgba8();
            let aa_image = image::load_from_memory(&aa.bytes).unwrap().to_rgba8();
            assert_eq!(aliased.dimensions(), aa_image.dimensions());
            assert!(aa_image.pixels().any(|p| p[3] > 0 && p[3] < 255));
            for y in [0, aa_image.height() - 1] {
                for x in 0..aa_image.width() {
                    assert_eq!(
                        aliased.get_pixel(x, y),
                        aa_image.get_pixel(x, y),
                        "boundary at ({x}, {y})"
                    );
                }
            }
        }
    }

    #[rstest]
    fn test_merge_node(#[values(false, true)] antialias: bool) {
        let colors = GraphColorSet::new(&GraphColorConfig::default());
        let params = ImageParams::new(&colors, CellWidthType::Double, antialias);
        let mut pixels = DrawingPixels::new(&params);
        // the merge in the first column, the merged line coming in from the second: ─╮
        let from_right = [
            Edge::new(EdgeType::Right, 0, 0),
            Edge::new(EdgeType::RightTop, 1, 0),
        ];
        let from_left = [
            Edge::new(EdgeType::LeftTop, 0, 0),
            Edge::new(EdgeType::Left, 1, 0),
        ];
        assert_eq!(Chevron::of(Fold::Unfolded, &from_right, 0), Chevron::Right);
        assert_eq!(Chevron::of(Fold::Unfolded, &from_left, 1), Chevron::Left);
        assert_eq!(Chevron::of(Fold::Folded, &[], 0), Chevron::Down);

        let mut pixel = |merge, pos_x, edges: &[Edge], (x, y)| {
            let row = calc_graph_row_image(
                pos_x,
                2,
                edges,
                &params,
                &mut pixels,
                GraphStyle::Rounded,
                merge,
            );
            *image::load_from_memory(&row.bytes)
                .unwrap()
                .to_rgba8()
                .get_pixel(x, y)
        };
        let color = params.edge_color(0);
        // a commit's dot is filled, a merge's ring hollow, larger than the dot
        assert_eq!(pixel(None, 0, &[], (25, 25)), color);
        assert_ne!(pixel(Some(Fold::Folded), 0, &[], (25, 25)), color);
        assert_ne!(pixel(None, 0, &[], (25, 25 - 16)), color);
        assert_eq!(pixel(Some(Fold::Folded), 0, &[], (25, 25 - 16)), color);
        // the starts of the chevrons, up the left of `>`, the right of `<`, the left of `v`
        let (right, left, down) = ((22, 19), (28, 19), (19, 22));
        assert_eq!(pixel(Some(Fold::Unfolded), 0, &from_right, right), color);
        assert_ne!(pixel(Some(Fold::Unfolded), 0, &from_right, down), color);
        assert_eq!(pixel(Some(Fold::Folded), 0, &[], down), color);
        assert_ne!(pixel(Some(Fold::Folded), 0, &[], right), color);
        let color = params.edge_color(1);
        let in_second = |(x, y)| (50 + x, y);
        assert_eq!(
            pixel(Some(Fold::Unfolded), 1, &from_left, in_second(left)),
            color
        );
        assert_ne!(
            pixel(Some(Fold::Unfolded), 1, &from_left, in_second(right)),
            color
        );
    }

    #[rstest]
    fn test_antialias_circle_colors(#[values("#ffffff90", "#00000000")] edge: &str) {
        let config = GraphColorConfig {
            branches: vec!["#c8c86480".into()],
            edge: edge.into(),
            background: "#00ff0070".into(),
        };
        let colors = GraphColorSet::new(&config);
        let params = ImageParams::new(&colors, CellWidthType::Double, true);
        let mut pixels = DrawingPixels::new(&params);
        let row = calc_graph_row_image(0, 1, &[], &params, &mut pixels, GraphStyle::Rounded, None);
        let image = image::load_from_memory(&row.bytes).unwrap().to_rgba8();
        assert_eq!(*image.get_pixel(25, 25), params.edge_color(0));
        let outline = if params.circle_edge_color[3] == 0 {
            params.background_color
        } else {
            params.circle_edge_color
        };
        assert_eq!(*image.get_pixel(25, 13), outline);
    }

    #[rustfmt::skip]
    fn simple_test_params() -> Vec<TestParam> {
        vec![
            (1, vec![(LeftBottom, 0, 0), (Left, 1, 0), (Down, 1, 1), (Right, 1, 3), (Horizontal, 2, 3), (RightBottom, 3, 3)]),
            (3, vec![(Vertical, 0, 0), (Up, 3, 3), (Down, 3, 3)]),
            (2, vec![(LeftTop, 0, 0), (Horizontal, 1, 0), (Left, 2, 0), (Up, 2, 2), (Right, 2, 3), (RightTop, 3, 3)]),
        ]
    }

    #[rustfmt::skip]
    fn straight_test_params() -> Vec<TestParam> {
        vec![
            (0, vec![(Up, 0, 0), (Down, 0, 0)]),
            (0, vec![(Up, 0, 0), (Down, 0, 0), (Right, 0, 1), (RightBottom, 1, 1)]),
            (1, vec![(Vertical, 0, 0), (Up, 1, 1), (Down, 1, 1)]),
            (0, vec![(Up, 0, 0), (Down, 0, 0), (Right, 0, 1), (RightTop, 1, 1)]),
        ]
    }

    #[rustfmt::skip]
    fn branches_test_params() -> Vec<TestParam> {
        vec![
            (0, vec![(Up, 0, 0), (Down, 0, 0),
                    (Right, 0, 1), (RightBottom, 1, 1),
                    (Right, 0, 2), (Horizontal, 1, 2), (RightBottom, 2, 2),
                    (Right, 0, 3), (Horizontal, 1, 3), (Horizontal, 2, 3), (RightBottom, 3, 3),
                    (Right, 0, 4), (Horizontal, 1, 4), (Horizontal, 2, 4), (Horizontal, 3, 4), (RightBottom, 4, 4),
                    (Right, 0, 5), (Horizontal, 1, 5), (Horizontal, 2, 5), (Horizontal, 3, 5), (Horizontal, 4, 5), (RightBottom, 5, 5),
                    (Right, 0, 6), (Horizontal, 1, 6), (Horizontal, 2, 6), (Horizontal, 3, 6), (Horizontal, 4, 6), (Horizontal, 5, 6), (RightBottom, 6, 6)]),
            (6, vec![(Vertical, 0, 0), (Vertical, 1, 1), (Vertical, 2, 2), (Vertical, 3, 3), (Vertical, 4, 4), (Vertical, 5, 5), (Down, 6, 6), (Up, 6, 6)]),
        ]
    }

    fn test_calc_graph_row_image(
        params: Vec<TestParam>,
        cell_count: usize,
        image_params: ImageParams,
        mut drawing_pixels: DrawingPixels,
        graph_style: GraphStyle,
        file_name: &str,
    ) {
        let graph_row_images: Vec<GraphRowImage> = params
            .into_iter()
            .map(|(commit_pos_x, edges)| {
                let edges: Vec<Edge> = edges
                    .into_iter()
                    .map(|t| Edge::new(t.0, t.1, t.2))
                    .collect();
                calc_graph_row_image(
                    commit_pos_x,
                    cell_count,
                    &edges,
                    &image_params,
                    &mut drawing_pixels,
                    graph_style,
                    None,
                )
            })
            .collect();

        let file_name = if image_params.antialias {
            format!("{file_name}_antialias")
        } else {
            file_name.to_owned()
        };
        let image = build_test_image(&graph_row_images, &image_params, cell_count);
        let result = compare_image_snapshot(
            &image,
            Path::new(SNAPSHOT_DIR),
            Path::new(OUTPUT_DIR),
            &file_name,
        );
        save_border_image(&image, &image_params, cell_count, &file_name);
        result.unwrap_or_else(|error| panic!("{error}"));
    }

    fn build_test_image(
        graph_row_images: &[GraphRowImage],
        image_params: &ImageParams,
        cell_count: usize,
    ) -> image::RgbaImage {
        let rows_len = graph_row_images.len() as u32;
        let image_width = image_params.width as u32 * cell_count as u32;
        let image_height = image_params.height as u32 * rows_len;

        let mut img_buf: image::ImageBuffer<image::Rgba<u8>, Vec<u8>> =
            image::ImageBuffer::new(image_width, image_height);

        for (i, graph_row_image) in graph_row_images.iter().enumerate() {
            let image = image::load_from_memory(&graph_row_image.bytes).unwrap();
            let y = image_params.height as u32 * (rows_len - (i as u32) - 1);
            img_buf.copy_from(&image, 0, y).unwrap();
        }

        img_buf
    }

    fn save_border_image(
        image: &image::RgbaImage,
        image_params: &ImageParams,
        cell_count: usize,
        file_name: &str,
    ) {
        let mut bordered_image = image.clone();
        for y in (0..image.height()).step_by(image_params.height as usize) {
            for x in 0..cell_count {
                let x_offset = x as u32 * image_params.width as u32;
                draw_border(&mut bordered_image, image_params, x_offset, y);
            }
        }

        bordered_image
            .save(Path::new(OUTPUT_DIR).join(format!("{file_name}_borders.png")))
            .unwrap();
    }

    fn draw_border(
        img_buf: &mut image::ImageBuffer<image::Rgba<u8>, Vec<u8>>,
        image_params: &ImageParams,
        x_offset: u32,
        y_offset: u32,
    ) {
        for x in 0..image_params.width {
            for y in 0..image_params.height {
                if x == 0 || x == image_params.width - 1 || y == 0 || y == image_params.height - 1 {
                    img_buf.put_pixel(
                        x as u32 + x_offset,
                        y as u32 + y_offset,
                        image::Rgba([255, 0, 0, 50]),
                    );
                }
            }
        }
    }
}
