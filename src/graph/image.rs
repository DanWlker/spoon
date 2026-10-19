use std::{
    fmt::{self, Debug, Formatter},
    hash::{Hash, Hasher},
    io::Cursor,
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

mod antialias;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphStyle {
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
    prepared_image_map: FxHashMap<CommitHash, PreparedImage>,
    image_ids: FxHashSet<u32>,
    pending_uploads: Vec<String>,

    graph: Graph<'a>,
    cell_width_type: CellWidthType,
    graph_style: GraphStyle,
    image_width_mode: GraphImageWidthMode,
    image_params: ImageParams,
    drawing_pixels: DrawingPixels,
    image_protocol: ImageProtocol,
    session_nonce: u32,
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
            image_ids: FxHashSet::default(),
            pending_uploads: Vec::default(),
            graph,
            cell_width_type,
            graph_style,
            image_width_mode,
            image_params,
            drawing_pixels,
            image_protocol,
            session_nonce: create_session_nonce(),
        }
    }

    pub fn prepared_image(&self, commit_hash: &CommitHash) -> &PreparedImage {
        self.prepared_image_map.get(commit_hash).unwrap()
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
        std::mem::take(&mut self.pending_uploads)
    }

    pub fn ensure_uploaded(&mut self, commit_hash: &CommitHash) {
        if self.prepared_image_map.contains_key(commit_hash) {
            return;
        }
        let image_id = graph_image_id(self.session_nonce, commit_hash);
        let graph_row_image = build_single_graph_row_image(
            &self.graph,
            &self.image_params,
            &mut self.drawing_pixels,
            self.graph_style,
            self.image_width_mode,
            commit_hash,
        );
        let mut image =
            graph_row_image.prepare(self.cell_width_type, self.image_protocol, image_id);
        if let Some(upload_data) = image.take_upload_data() {
            self.pending_uploads.push(upload_data);
        }
        self.prepared_image_map.insert(commit_hash.clone(), image);
        self.image_ids.insert(image_id);
    }
}

// The manager owns the images it uploaded: deleting them when it is dropped keeps a page switch or
// a replaced submodule panel from leaving them in the terminal until the app quits.
impl Drop for GraphImageManager<'_> {
    fn drop(&mut self) {
        let mut image_ids: Vec<u32> = self.image_ids.iter().copied().collect();
        image_ids.sort_unstable();
        let _ = self.image_protocol.delete_images(&image_ids);
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

fn graph_image_id(session_nonce: u32, commit_hash: &CommitHash) -> u32 {
    let mut hasher = rustc_hash::FxHasher::default();
    session_nonce.hash(&mut hasher);
    commit_hash.hash(&mut hasher);
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
    // shortcut: taken once at start, changing the font size meanwhile brings the gaps back.
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
    let (pos_x, pos_y) = graph.commit_pos_map[&commit_hash];
    let edges = &graph.edges[pos_y];

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
        let circle = calc_commit_circle_drawing_pixels(image_params);
        let circle_edge = calc_circle_edge_drawing_pixels(image_params);
        let vertical_edge = calc_vertical_edge_drawing_pixels(image_params);
        let horizontal_edge = calc_horizontal_edge_drawing_pixels(image_params);
        let up_edge = calc_up_edge_drawing_pixels(image_params);
        let down_edge = calc_down_edge_drawing_pixels(image_params);
        let left_edge = calc_left_edge_drawing_pixels(image_params);
        let right_edge = calc_right_edge_drawing_pixels(image_params);
        let right_top_edge = calc_right_top_edge_drawing_pixels(image_params);
        let left_top_edge = calc_left_top_edge_drawing_pixels(image_params);
        let right_bottom_edge = calc_right_bottom_edge_drawing_pixels(image_params);
        let left_bottom_edge = calc_left_bottom_edge_drawing_pixels(image_params);

        Self {
            diagonal_connections: FxHashMap::default(),
            curved_connections: FxHashMap::default(),
            antialias: image_params
                .antialias
                .then(|| antialias::Masks::new(image_params)),
            circle,
            circle_edge,
            merge_disk: cell_pixels(image_params, |p| merge_disk_contains(p, image_params)),
            merge_marks: Chevron::ALL
                .into_iter()
                .map(|chevron| {
                    let marks = cell_pixels(image_params, |p| {
                        merge_mark_contains(p, image_params, chevron)
                    });
                    (chevron, marks)
                })
                .collect(),
            vertical_edge,
            horizontal_edge,
            up_edge,
            down_edge,
            left_edge,
            right_edge,
            right_top_edge,
            left_top_edge,
            right_bottom_edge,
            left_bottom_edge,
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

fn calc_commit_circle_drawing_pixels(image_params: &ImageParams) -> Pixels {
    calc_circle_drawing_pixels(image_params, image_params.circle_inner_radius as i32)
}

fn calc_circle_edge_drawing_pixels(image_params: &ImageParams) -> Pixels {
    let inner = calc_circle_drawing_pixels(image_params, image_params.circle_inner_radius as i32);
    let outer = calc_circle_drawing_pixels(image_params, image_params.circle_outer_radius as i32);

    outer.difference(&inner).cloned().collect()
}

// The pixels of a cell whose center is in a shape.
fn cell_pixels(image_params: &ImageParams, contains: impl Fn(Point) -> bool) -> Pixels {
    let (width, height) = (image_params.width as i32, image_params.height as i32);
    (0..height)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .filter(|&(x, y)| contains(Point::new(f64::from(x) + 0.5, f64::from(y) + 0.5)))
        .collect()
}

fn calc_circle_drawing_pixels(image_params: &ImageParams, radius: i32) -> Pixels {
    // Bresenham's circle algorithm
    let center_x = (image_params.width / 2) as i32;
    let center_y = (image_params.height / 2) as i32;

    let mut x = radius;
    let mut y = 0;
    let mut p = 1 - radius;

    let mut pixels = Pixels::default();

    while x >= y {
        for dx in -x..=x {
            pixels.insert((center_x + dx, center_y + y));
            pixels.insert((center_x + dx, center_y - y));
        }
        for dx in -y..=y {
            pixels.insert((center_x + dx, center_y + x));
            pixels.insert((center_x + dx, center_y - x));
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

fn calc_vertical_edge_drawing_pixels(image_params: &ImageParams) -> Pixels {
    let center_x = (image_params.width / 2) as i32;
    let line_width = image_params.line_width as i32;
    let x_start = center_x - line_width / 2;

    let mut pixels = Pixels::default();
    for y in 0..image_params.height as i32 {
        for x in x_start..(x_start + line_width) {
            pixels.insert((x, y));
        }
    }
    pixels
}

fn calc_horizontal_edge_drawing_pixels(image_params: &ImageParams) -> Pixels {
    let center_y = (image_params.height / 2) as i32;
    let line_width = image_params.line_width as i32;
    let y_start = center_y - line_width / 2;

    let mut pixels = Pixels::default();
    for y in y_start..(y_start + line_width) {
        for x in 0..image_params.width as i32 {
            pixels.insert((x, y));
        }
    }
    pixels
}

fn calc_up_edge_drawing_pixels(image_params: &ImageParams) -> Pixels {
    let center_x = (image_params.width / 2) as i32;
    let line_width = image_params.line_width as i32;
    let x_start = center_x - line_width / 2;
    let circle_center_y = (image_params.height / 2) as i32;
    let circle_outer_radius = image_params.circle_outer_radius as i32;

    let mut pixels = Pixels::default();
    for y in 0..(circle_center_y - circle_outer_radius) {
        for x in x_start..(x_start + line_width) {
            pixels.insert((x, y));
        }
    }
    pixels
}

fn calc_down_edge_drawing_pixels(image_params: &ImageParams) -> Pixels {
    let center_x = (image_params.width / 2) as i32;
    let line_width = image_params.line_width as i32;
    let x_start = center_x - line_width / 2;
    let circle_center_y = (image_params.height / 2) as i32;
    let circle_outer_radius = image_params.circle_outer_radius as i32;

    let mut pixels = Pixels::default();
    for y in (circle_center_y + circle_outer_radius + 1)..(image_params.height as i32) {
        for x in x_start..(x_start + line_width) {
            pixels.insert((x, y));
        }
    }
    pixels
}

fn calc_left_edge_drawing_pixels(image_params: &ImageParams) -> Pixels {
    let center_y = (image_params.height / 2) as i32;
    let line_width = image_params.line_width as i32;
    let y_start = center_y - line_width / 2;
    let circle_center_x = (image_params.width / 2) as i32;
    let circle_outer_radius = image_params.circle_outer_radius as i32;

    let mut pixels = Pixels::default();
    for y in y_start..(y_start + line_width) {
        for x in 0..(circle_center_x - circle_outer_radius) {
            pixels.insert((x, y));
        }
    }
    pixels
}

fn calc_right_edge_drawing_pixels(image_params: &ImageParams) -> Pixels {
    let center_y = (image_params.height / 2) as i32;
    let line_width = image_params.line_width as i32;
    let y_start = center_y - line_width / 2;
    let circle_center_x = (image_params.width / 2) as i32;
    let circle_outer_radius = image_params.circle_outer_radius as i32;

    let mut pixels = Pixels::default();
    for y in y_start..(y_start + line_width) {
        for x in (circle_center_x + circle_outer_radius + 1)..(image_params.width as i32) {
            pixels.insert((x, y));
        }
    }
    pixels
}

fn calc_right_top_edge_drawing_pixels(image_params: &ImageParams) -> Pixels {
    let (w, h, r) = (
        image_params.width as i32,
        image_params.height as i32,
        image_params.corner_radius() as i32,
    );
    let (x_offset, y_offset) = if w < h {
        (0, r - (h / 2))
    } else {
        ((w / 2) - r, 0)
    };
    calc_corner_edge_drawing_pixels(image_params, 0, h, x_offset, y_offset)
}

fn calc_left_top_edge_drawing_pixels(image_params: &ImageParams) -> Pixels {
    let (w, h, r) = (
        image_params.width as i32,
        image_params.height as i32,
        image_params.corner_radius() as i32,
    );
    let (x_offset, y_offset) = if w < h {
        (0, r - (h / 2))
    } else {
        (r - (w / 2), 0)
    };
    calc_corner_edge_drawing_pixels(image_params, w, h, x_offset, y_offset)
}

fn calc_right_bottom_edge_drawing_pixels(image_params: &ImageParams) -> Pixels {
    let (w, h, r) = (
        image_params.width as i32,
        image_params.height as i32,
        image_params.corner_radius() as i32,
    );
    let (x_offset, y_offset) = if w < h {
        (0, (h / 2) - r)
    } else {
        ((w / 2) - r, 0)
    };
    calc_corner_edge_drawing_pixels(image_params, 0, 0, x_offset, y_offset)
}

fn calc_left_bottom_edge_drawing_pixels(image_params: &ImageParams) -> Pixels {
    let (w, h, r) = (
        image_params.width as i32,
        image_params.height as i32,
        image_params.corner_radius() as i32,
    );
    let (x_offset, y_offset) = if w < h {
        (0, (h / 2) - r)
    } else {
        (r - (w / 2), 0)
    };
    calc_corner_edge_drawing_pixels(image_params, w, 0, x_offset, y_offset)
}

fn calc_corner_edge_drawing_pixels(
    image_params: &ImageParams,
    base_center_x: i32,
    base_center_y: i32,
    x_offset: i32,
    y_offset: i32,
) -> Pixels {
    // Bresenham's circle algorithm
    let curve_center_x = base_center_x;
    let curve_center_y = base_center_y;
    let line_width = image_params.line_width as i32;
    let half_line_width = line_width / 2;
    let adjust = if image_params.line_width.is_multiple_of(2) {
        0
    } else {
        1
    };
    let radius_base_length = image_params.corner_radius() as i32;
    let inner_radius = radius_base_length - half_line_width - adjust;
    let outer_radius = radius_base_length + half_line_width;

    let mut x = inner_radius;
    let mut y = 0;
    let mut p = 1 - inner_radius;

    let mut inner_pixels = Pixels::default();

    while x >= y {
        for dx in -x..=x {
            inner_pixels.insert((curve_center_x + dx, curve_center_y + y));
            inner_pixels.insert((curve_center_x + dx, curve_center_y - y));
        }
        for dx in -y..=y {
            inner_pixels.insert((curve_center_x + dx, curve_center_y + x));
            inner_pixels.insert((curve_center_x + dx, curve_center_y - x));
        }

        y += 1;
        if p <= 0 {
            p += 2 * y + 1;
        } else {
            x -= 1;
            p += 2 * y - 2 * x + 1;
        }
    }

    let mut x = outer_radius;
    let mut y = 0;
    let mut p = 1 - outer_radius;

    let mut outer_pixels = Pixels::default();

    while x >= y {
        for dx in -x..=x {
            outer_pixels.insert((curve_center_x + dx, curve_center_y + y));
            outer_pixels.insert((curve_center_x + dx, curve_center_y - y));
        }
        for dx in -y..=y {
            outer_pixels.insert((curve_center_x + dx, curve_center_y + x));
            outer_pixels.insert((curve_center_x + dx, curve_center_y - x));
        }

        y += 1;
        if p <= 0 {
            p += 2 * y + 1;
        } else {
            x -= 1;
            p += 2 * y - 2 * x + 1;
        }
    }

    let mut pixels: Pixels = outer_pixels
        .difference(&inner_pixels)
        .filter(|p| {
            p.0 >= 0
                && p.0 < image_params.width as i32
                && p.1 >= 0
                && p.1 < image_params.height as i32
        })
        .map(|p| (p.0 + x_offset, p.1 + y_offset))
        .collect();

    if image_params.width < image_params.height {
        let (ys, ye) = if y_offset < 0 {
            (base_center_y + y_offset, base_center_y)
        } else {
            (base_center_y, base_center_y + y_offset)
        };
        let center_x = (image_params.width / 2) as i32;
        let x_start = center_x - line_width / 2;
        for x in x_start..(x_start + line_width) {
            for y in ys..ye {
                pixels.insert((x, y));
            }
        }
    }
    if image_params.width > image_params.height {
        let (xs, xe) = if x_offset < 0 {
            (base_center_x + x_offset, base_center_x)
        } else {
            (base_center_x, base_center_x + x_offset)
        };
        let center_y = (image_params.height / 2) as i32;
        let y_start = center_y - line_width / 2;
        for y in y_start..(y_start + line_width) {
            for x in xs..xe {
                pixels.insert((x, y));
            }
        }
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

    match graph_style {
        GraphStyle::Curved => {
            for edge in edges.iter().filter(|e| e.edge_type.is_vertically_related()) {
                draw_edge(&mut img_buf, edge, image_params, drawing_pixels);
            }
            draw_curved_connected_edge(&mut img_buf, edges, image_params, drawing_pixels);
        }
        GraphStyle::Rounded => {
            for edge in edges {
                draw_edge(&mut img_buf, edge, image_params, drawing_pixels)
            }
        }
        GraphStyle::Angular => {
            let (vertial_edges, horizontal_edges): (Vec<&Edge>, Vec<&Edge>) = edges
                .iter()
                .partition(|e| e.edge_type.is_vertically_related());
            for edge in vertial_edges {
                draw_edge(&mut img_buf, edge, image_params, drawing_pixels)
            }
            let mut horizontal_edges_map: FxHashMap<usize, Vec<&Edge>> = FxHashMap::default();
            for edge in horizontal_edges {
                horizontal_edges_map
                    .entry(edge.associated_line_pos_x)
                    .or_default()
                    .push(edge);
            }
            for edges in horizontal_edges_map.values() {
                draw_diagonal_connected_edge(&mut img_buf, edges, image_params, drawing_pixels);
            }
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
            for (x, y) in pixels {
                img_buf.put_pixel((x + x_offset) as u32, *y as u32, color);
            }
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
    img_buf: &mut image::ImageBuffer<image::Rgba<u8>, Vec<u8>>,
    circle_pos_x: usize,
    image_params: &ImageParams,
    drawing_pixels: &DrawingPixels,
) {
    let x_offset = (circle_pos_x * image_params.width as usize) as i32;
    let color = image_params.edge_color(circle_pos_x);

    for (x, y) in &drawing_pixels.circle {
        let x = (*x + x_offset) as u32;
        let y = *y as u32;

        let pixel = img_buf.get_pixel_mut(x, y);
        *pixel = color;
    }

    if image_params.circle_edge_color[3] == 0 {
        // If the alpha value is 0, the circle edge is transparent, so we don't need to draw it.
        return;
    }

    for (x, y) in &drawing_pixels.circle_edge {
        let x = (*x + x_offset) as u32;
        let y = *y as u32;

        let pixel = img_buf.get_pixel_mut(x, y);
        *pixel = image_params.circle_edge_color;
    }
}

fn draw_edge(
    img_buf: &mut image::ImageBuffer<image::Rgba<u8>, Vec<u8>>,
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

    for (x, y) in pixels {
        let x = (*x + x_offset) as u32;
        let y = *y as u32;

        let pixel = img_buf.get_pixel_mut(x, y);
        *pixel = color;
    }
}

fn draw_diagonal_connected_edge(
    img_buf: &mut image::ImageBuffer<image::Rgba<u8>, Vec<u8>>,
    edges: &[&Edge],
    image_params: &ImageParams,
    drawing_pixels: &mut DrawingPixels,
) {
    for corner in edges
        .iter()
        .filter(|e| !e.edge_type.is_vertically_related())
    {
        let side_type = match corner.edge_type {
            EdgeType::RightBottom | EdgeType::RightTop => EdgeType::Right,
            EdgeType::LeftBottom | EdgeType::LeftTop => EdgeType::Left,
            _ => continue,
        };
        let Some(side) = edges.iter().find(|e| e.edge_type == side_type) else {
            continue;
        };
        let color = image_params.edge_color(side.associated_line_pos_x);
        let x_offset = (side.pos_x.min(corner.pos_x) * image_params.width as usize) as i32;
        let pixels = drawing_pixels.diagonal_connection_pixels(side, corner, image_params);
        draw_connection_pixels(img_buf, pixels, x_offset, color);
    }
}

fn draw_connection_pixels(
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

fn draw_curved_connected_edge(
    img_buf: &mut image::ImageBuffer<image::Rgba<u8>, Vec<u8>>,
    edges: &[Edge],
    image_params: &ImageParams,
    drawing_pixels: &mut DrawingPixels,
) {
    let corner_edges = edges.iter().filter(|e| {
        matches!(
            e.edge_type,
            EdgeType::RightBottom | EdgeType::LeftBottom | EdgeType::RightTop | EdgeType::LeftTop
        )
    });

    for corner in corner_edges {
        let side_type = match corner.edge_type {
            EdgeType::RightBottom | EdgeType::RightTop => EdgeType::Right,
            _ => EdgeType::Left,
        };
        let Some(side) = edges.iter().find(|e| {
            e.associated_line_pos_x == corner.associated_line_pos_x && e.edge_type == side_type
        }) else {
            continue;
        };

        let color = image_params.edge_color(corner.associated_line_pos_x);
        let x_offset = (side.pos_x.min(corner.pos_x) * image_params.width as usize) as i32;
        let pixels = drawing_pixels.curved_connection_pixels(side, corner, image_params);
        draw_connection_pixels(img_buf, pixels, x_offset, color);
    }
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

fn calc_curved_connection_drawing_pixels(
    side: &Edge,
    corner: &Edge,
    params: &ImageParams,
) -> Pixels {
    let points = curved_connection_points(side, corner, params);
    let radius = f64::from(params.line_width) / 2.0;
    let mut pixels = Pixels::default();
    let direction = (points[3].x - points[0].x).signum();
    let steps = (((points[3].x - points[0].x).abs() + (points[3].y - points[0].y).abs()) * 3.0)
        .ceil() as usize;
    let boundary_y = if points[3].y == 0.0 {
        0
    } else {
        u32::from(params.height) - 1
    };
    // Approximate the curve with short segments and fill pixels within the stroke.
    let steps = steps.max(32);
    let mut start = points[0];
    for step in 1..=steps {
        let end = bezier_point(&points, step as f64 / steps as f64);
        let segment = end - start;
        let length_squared = segment.dot(segment);
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
                if (pixel_center.x - points[0].x) * direction < 0.0 {
                    continue;
                }
                let t = if length_squared == 0.0 {
                    0.0
                } else {
                    ((pixel_center - start).dot(segment) / length_squared).clamp(0.0, 1.0)
                };
                let distance = (pixel_center - (start + segment * t)).length();
                if distance <= radius {
                    pixels.insert((x as i32, y as i32));
                }
            }
        }
        start = end;
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

    use crate::{config::GraphColorConfig, test_support::image_snapshot::compare_image_snapshot};

    use super::*;

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
    use EdgeType::*;

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
            let mut edges: Vec<_> = edges
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
            edges.reverse();
            let reordered =
                calc_graph_row_image(pos, 4, &edges, &aa_params, &mut aa_pixels, style, None);
            assert_eq!(aa.bytes, reordered.bytes);
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
