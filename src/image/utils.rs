use std::collections::VecDeque;

pub struct FloodFiller {
    visited: VecDeque<(isize, isize)>,
}

#[derive(PartialEq)]
pub enum FillPtSetResult {
    Hit,
    Miss,
    Error
}

impl FloodFiller {
    pub fn new() -> FloodFiller {
        FloodFiller {
            visited: VecDeque::new(),
        }
    }

    pub fn fill<SetFilled: FnMut(isize, isize) -> FillPtSetResult>(
        &mut self,
        x: isize,
        y: isize,
        mut try_set_filled: SetFilled
    ) -> bool {
        match try_set_filled(x, y) {
            FillPtSetResult::Miss => return true,
            FillPtSetResult::Error=> return false,
            _ => {},
        };

        self.visited.clear();
        self.visited.push_back((x, y));

        let mut error_flag = false;
        while let Some((pt_x, pt_y)) = self.visited.pop_front() {
            let mut check_neibour = |x, y| {
                let result = try_set_filled(x, y);
                if result == FillPtSetResult::Error {
                    error_flag = true;
                }
                if result != FillPtSetResult::Hit { return; }
                self.visited.push_back((x, y));
            };
            for dx in -1..=1 {
                for dy in -1..=1 {
                    if dx == 0 && dy == 0 { continue; }
                    check_neibour(pt_x + dx, pt_y + dy);
                }
            }
            if error_flag { return false; }
        }
        true
    }
}
