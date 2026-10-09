use vt100::Screen;

use crate::motion::Pos;
use crate::vt;

/// The program's cursor in view n's coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Live {
    pub cursor: Pos,
    /// view n's `top` that shows the program's screen
    pub bottom: usize,
}

pub fn live(screen: &mut Screen) -> Live {
    let bottom = vt::history(screen);
    let (row, col) = screen.cursor_position();
    Live { cursor: (bottom + row as usize, vt::index(screen, row, col)), bottom }
}

/// View n's cursor and top after output. At the bottom with the cursor on or below the program's line, it keeps up
/// with the program; once you move up or scroll back, the text stays put.
pub fn follow(cursor: Pos, top: usize, before: Live, after: Live) -> (Pos, usize) {
    if top < before.bottom || cursor.0 < before.cursor.0 {
        return (cursor, top);
    }
    let cursor = if after.cursor != before.cursor { after.cursor } else { cursor };
    (cursor, after.bottom)
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn live(cursor: Pos, bottom: usize) -> Live {
        Live { cursor, bottom }
    }

    #[test]
    fn keeps_up_before_the_screen_fills() {
        assert_eq!(follow((0, 2), 0, live((0, 2), 0), live((5, 0), 0)), ((5, 0), 0));
    }

    #[test]
    fn keeps_up_as_output_scrolls() {
        assert_eq!(follow((30, 0), 7, live((30, 0), 7), live((42, 3), 19)), ((42, 3), 19));
    }

    #[test]
    fn stays_put_once_moved_up_or_scrolled() {
        assert_eq!(follow((29, 4), 7, live((30, 0), 7), live((42, 0), 19)), ((29, 4), 7));
        assert_eq!(follow((30, 0), 5, live((30, 0), 7), live((42, 0), 19)), ((30, 0), 5));
    }

    #[test]
    fn below_the_program_counts_as_following() {
        assert_eq!(follow((35, 0), 7, live((30, 0), 7), live((31, 0), 7)), ((31, 0), 7));
    }

    #[test]
    fn a_redraw_in_place_keeps_your_column() {
        assert_eq!(follow((30, 4), 7, live((30, 9), 7), live((30, 9), 7)), ((30, 4), 7));
    }
}
