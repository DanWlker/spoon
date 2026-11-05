use crate::{
    graph::{CellWidthType, Graph},
    GraphWidthType, Result,
};

// `size` is the terminal's (columns, rows), which the graph must fit; `page` the columns of the
// commits panel beside the refs sidebar, which `auto` fits double cells in.
pub fn decide_cell_width_type(
    graph: &Graph,
    (w, h): (u16, u16),
    page: u16,
    cell_width_type: Option<GraphWidthType>,
) -> Result<CellWidthType> {
    decide_cell_width_type_from(
        graph.max_pos_x,
        w as usize,
        h as usize,
        page as usize,
        cell_width_type,
    )
}

fn decide_cell_width_type_from(
    max_pos_x: usize,
    term_width: usize,
    term_height: usize,
    page_width: usize,
    cell_width_type: Option<GraphWidthType>,
) -> Result<CellWidthType> {
    let single_image_cell_width = max_pos_x + 1;
    let double_image_cell_width = single_image_cell_width * 2;

    match cell_width_type {
        Some(GraphWidthType::Double) => {
            let required_width = double_image_cell_width + 2;
            if required_width > term_width {
                let msg = format!("Terminal too small ({term_width}x{term_height} characters). The current graph needs at least {required_width} columns to display properly.");
                return Err(msg.into());
            }
            Ok(CellWidthType::Double)
        }
        Some(GraphWidthType::Single) => {
            let required_width = single_image_cell_width + 2;
            if required_width > term_width {
                let msg = format!("Terminal too small ({term_width}x{term_height} characters). The current graph needs at least {required_width} columns to display properly.");
                return Err(msg.into());
            }
            Ok(CellWidthType::Single)
        }
        Some(GraphWidthType::Auto) | None => {
            let double_required_width = double_image_cell_width + 2;
            if double_required_width <= page_width {
                return Ok(CellWidthType::Double);
            }
            let single_required_width = single_image_cell_width + 2;
            if single_required_width <= term_width {
                return Ok(CellWidthType::Single);
            }
            let msg = format!("Terminal too small ({term_width}x{term_height} characters). The current graph needs at least {single_required_width} columns to display properly.");
            Err(msg.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auto_fits_double_cells_in_the_commits_panel() {
        // a graph 30 cells wide: double cells take 62 columns with the pad, single ones 32
        let decide = |term, page, kind| decide_cell_width_type_from(29, term, 40, page, kind);
        // the terminal has room for double cells, the commits panel beside the sidebar doesn't
        assert_eq!(decide(70, 42, None).unwrap(), CellWidthType::Single);
        assert_eq!(decide(100, 72, None).unwrap(), CellWidthType::Double);
        // asked for, double cells only need the terminal's room, cut beside the sidebar
        assert_eq!(
            decide(70, 42, Some(GraphWidthType::Double)).unwrap(),
            CellWidthType::Double
        );
        // too narrow for single cells is refused, whatever the panel
        assert!(decide(20, 0, None).is_err());
    }
}
