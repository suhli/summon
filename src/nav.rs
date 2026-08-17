#[derive(Debug, Clone, Copy)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

pub fn move_selection(current: i32, count: i32, columns: i32, direction: Direction) -> i32 {
    if count <= 0 {
        return -1;
    }
    let columns = columns.max(1);
    let current = current.clamp(0, count - 1);

    match direction {
        Direction::Left => {
            if current > 0 {
                current - 1
            } else {
                current
            }
        }
        Direction::Right => {
            if current + 1 < count {
                current + 1
            } else {
                current
            }
        }
        Direction::Up => {
            let next = current - columns;
            if next >= 0 {
                next
            } else {
                current
            }
        }
        Direction::Down => {
            let next = current + columns;
            if next < count {
                next
            } else {
                let last_row_start = ((count - 1) / columns) * columns;
                if current < last_row_start {
                    let col = current % columns;
                    let target = last_row_start + col;
                    if target < count {
                        target
                    } else {
                        count - 1
                    }
                } else {
                    current
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_navigation_edges() {
        // 0 1 2 3 4 5
        // 6 7 8 9 10 11
        // 12 13 14
        assert_eq!(move_selection(0, 15, 6, Direction::Left), 0);
        assert_eq!(move_selection(0, 15, 6, Direction::Up), 0);
        assert_eq!(move_selection(14, 15, 6, Direction::Right), 14);
        assert_eq!(move_selection(5, 15, 6, Direction::Down), 11);
        assert_eq!(move_selection(11, 15, 6, Direction::Down), 14);
        assert_eq!(move_selection(8, 15, 6, Direction::Down), 14);
        assert_eq!(move_selection(12, 15, 6, Direction::Up), 6);
    }

    #[test]
    fn empty_grid() {
        assert_eq!(move_selection(0, 0, 6, Direction::Right), -1);
    }
}
