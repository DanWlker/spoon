use crate::{
    config::GraphWidthType,
    graph::{CellWidthType, Graph},
};

// Double cells where asked for, or where `auto` fits them in `page`, the columns of the commits
// panel beside the refs sidebar; else single ones. A graph wider than the terminal is cut, as on
// a resize: it used to stop the app at the start only.
pub fn decide_cell_width_type(
    graph: &Graph,
    page: u16,
    cell_width_type: Option<GraphWidthType>,
) -> CellWidthType {
    decide_cell_width_type_from(graph.max_pos_x, page as usize, cell_width_type)
}

fn decide_cell_width_type_from(
    max_pos_x: usize,
    page_width: usize,
    cell_width_type: Option<GraphWidthType>,
) -> CellWidthType {
    // two columns a cell, with the pad
    let double_fits = (max_pos_x + 1) * 2 + 2 <= page_width;
    match cell_width_type {
        Some(GraphWidthType::Double) => CellWidthType::Double,
        Some(GraphWidthType::Single) => CellWidthType::Single,
        Some(GraphWidthType::Auto) | None if double_fits => CellWidthType::Double,
        Some(GraphWidthType::Auto) | None => CellWidthType::Single,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auto_fits_double_cells_in_the_commits_panel() {
        // a graph 30 cells wide: double cells take 62 columns with the pad
        let decide = |page, kind| decide_cell_width_type_from(29, page, kind);
        // the commits panel beside the sidebar has no room for double cells, then has
        assert_eq!(decide(42, None), CellWidthType::Single);
        assert_eq!(decide(72, None), CellWidthType::Double);
        // asked for, they are taken whatever the room, cut
        assert_eq!(
            decide(42, Some(GraphWidthType::Double)),
            CellWidthType::Double
        );
        // too narrow for single cells too: cut, as on a resize; it was refused at the start
        assert_eq!(decide(0, None), CellWidthType::Single);
    }
}
