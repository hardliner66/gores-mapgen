use dt::num::ToPrimitive;
use ndarray::{s, Array2};
use serde::de::Error;

use crate::{
    config::{GenerationConfig, MapConfig},
    generator,
    kernel::Kernel,
    map::{BlockType, Map, Overwrite},
    position::{Position, ShiftDirection},
    random::Random,
};

// this walker is indeed very cute
#[derive(Debug)]
pub struct CuteWalker {
    pub pos: Position,
    pub steps: usize,
    pub inner_kernel: Kernel,
    pub outer_kernel: Kernel,
    pub goal: Option<Position>,
    pub goal_index: usize,
    pub waypoints: Vec<Position>,

    /// indicates whether walker has reached the last waypoint
    pub finished: bool,

    /// keeps track of how many steps ago the last platorm has been placed
    pub steps_since_platform: usize,

    /// keeps track of the last shift direction
    pub last_shift: Option<ShiftDirection>,

    /// counts how many steps the pulse constraints have been fulfilled
    pub pulse_counter: usize,

    /// keeps track on which positions can no longer be visited
    pub locked_positions: Array2<bool>,

    /// keeps track of all positions the walker has visited so far
    pub position_history: Vec<Position>,
}

impl CuteWalker {
    pub fn new(
        initial_pos: Position,
        inner_kernel: Kernel,
        outer_kernel: Kernel,
        waypoints: Vec<Position>,
        map: &Map,
    ) -> CuteWalker {
        CuteWalker {
            pos: initial_pos,
            steps: 0,
            inner_kernel,
            outer_kernel,
            goal: Some(waypoints.first().unwrap().clone()),
            goal_index: 0,
            waypoints,
            finished: false,
            steps_since_platform: 0,
            last_shift: None,
            pulse_counter: 0,
            locked_positions: Array2::from_elem((map.width, map.height), false),
            position_history: Vec::new(),
        }
    }

    pub fn is_goal_reached(&self, waypoint_reached_dist: &usize) -> Option<bool> {
        self.goal
            .as_ref()
            .map(|goal| goal.distance_squared(&self.pos) <= *waypoint_reached_dist)
    }

    pub fn next_waypoint(&mut self) {
        if let Some(next_goal) = self.waypoints.get(self.goal_index + 1) {
            self.goal_index += 1;
            self.goal = Some(next_goal.clone());
        } else {
            self.finished = true;
            self.goal = None;
        }
    }

    /// will try to place a platform at the walkers position.
    /// If force is true it will enforce a platform.
    pub fn check_platform(
        &mut self,
        map: &mut Map,
        min_distance: usize,
        max_distance: usize,
    ) -> Result<(), &'static str> {
        self.steps_since_platform += 1;

        // Case 1: min distance is not reached -> skip
        if self.steps_since_platform < min_distance {
            return Ok(());
        }

        let walker_pos = self.pos.clone();

        // Case 2: max distance has been exceeded -> force platform using a room
        if self.steps_since_platform > max_distance {
            generator::generate_room(map, &walker_pos.shifted_by(0, 6)?, 5, 3, None)?;
            self.steps_since_platform = 0;
            return Ok(());
        }

        // Case 3: min distance has been exceeded -> Try to place platform, but only if possible
        let area_empty = map.check_area_all(
            &walker_pos.shifted_by(-3, -3)?,
            &walker_pos.shifted_by(3, 2)?,
            &BlockType::Empty,
        )?;
        if area_empty {
            map.set_area(
                &walker_pos.shifted_by(-1, 0)?,
                &walker_pos.shifted_by(1, 0)?,
                &BlockType::Platform,
                &Overwrite::ReplaceEmptyOnly,
            );
            self.steps_since_platform = 0;
        }

        Ok(())
    }

    pub fn probabilistic_step(
        &mut self,
        map: &mut Map,
        gen_config: &GenerationConfig,
        rnd: &mut Random,
    ) -> Result<(), &'static str> {
        if self.finished {
            return Err("Walker is finished");
        }

        // save position to history before its updated
        self.position_history.push(self.pos.clone());

        // sample next shift
        let goal = self.goal.as_ref().ok_or("Error: Goal is None")?;
        let shifts = self.pos.get_rated_shifts(goal, map);

        let mut current_shift = rnd.sample_shift(&shifts);

        // Momentum: re-use last shift direction with certain probability
        if let Some(last_shift) = self.last_shift {
            if rnd.with_probability(gen_config.momentum_prob) {
                current_shift = last_shift;
            }
        }

        let mut current_target_pos = self.pos.clone();
        current_target_pos.shift_in_direction(&current_shift, map)?;

        // if target pos is locked, re-sample until a valid one is found
        let mut invalid = false;
        for _ in 0..100 {
            invalid = self.locked_positions[current_target_pos.as_index()];

            if invalid {
                current_shift = rnd.sample_shift(&shifts);
                current_target_pos = self.pos.clone();
                current_target_pos.shift_in_direction(&current_shift, map)?;
            }
        }

        if invalid {
            return Err("Walker got stuck :(");
        }

        // determine if direction changed from last shift
        let same_dir = match self.last_shift {
            Some(last_shift) => current_shift == last_shift,
            None => false,
        };

        // apply selected shift
        self.pos.shift_in_direction(&current_shift, map)?;
        self.steps += 1;

        // lock old position
        self.lock_previous_location(100, map, gen_config);

        // perform pulse if config constraints allows it
        let perform_pulse = gen_config.enable_pulse
            && ((same_dir && self.pulse_counter > gen_config.pulse_straight_delay)
                || (!same_dir && self.pulse_counter > gen_config.pulse_corner_delay));

        // apply kernels
        if perform_pulse {
            self.pulse_counter = 0; // reset pulse counter
            map.apply_kernel(
                &self.pos,
                &Kernel::new(&self.inner_kernel.size + 4, 0.0),
                BlockType::Freeze,
            )?;
            map.apply_kernel(
                &self.pos,
                &Kernel::new(&self.inner_kernel.size + 2, 0.0),
                BlockType::Empty,
            )?;
        } else {
            map.apply_kernel(&self.pos, &self.outer_kernel, BlockType::Freeze)?;

            let empty = if self.steps < gen_config.fade_steps {
                BlockType::EmptyReserved
            } else {
                BlockType::Empty
            };
            map.apply_kernel(&self.pos, &self.inner_kernel, empty)?;
        };

        if same_dir && self.inner_kernel.size <= gen_config.pulse_max_kernel_size {
            self.pulse_counter += 1;
        } else {
            self.pulse_counter = 0;
        };

        self.last_shift = Some(current_shift.clone());

        Ok(())
    }

    pub fn cuddle(&self) {
        println!("Cute walker was cuddled!");
    }

    /// fades kernel size from max_size to min_size for fade_steps
    pub fn set_fade_kernel(
        &mut self,
        step: usize,
        min_size: usize,
        max_size: usize,
        fade_steps: usize,
    ) {
        let slope = (min_size as f32 - max_size as f32) / fade_steps as f32;
        let kernel_size_f = (step as f32) * slope + max_size as f32;
        let kernel_size = kernel_size_f.floor() as usize;
        self.inner_kernel = Kernel::new(kernel_size, 0.0);
        self.outer_kernel = Kernel::new(kernel_size + 2, 0.0);
    }

    pub fn mutate_kernel(&mut self, config: &GenerationConfig, rnd: &mut Random) {
        let mut inner_size = self.inner_kernel.size;
        let mut inner_circ = self.inner_kernel.circularity;
        let mut outer_size = self.outer_kernel.size;
        let mut outer_circ = self.outer_kernel.circularity;
        let mut outer_margin = outer_size - inner_size;
        let mut modified = false;

        if rnd.with_probability(config.inner_size_mut_prob) {
            inner_size = rnd.sample_inner_kernel_size();
            modified = true;
        } else {
            rnd.skip_n(2); // for some reason sampling requires two values?
        }

        if rnd.with_probability(config.outer_size_mut_prob) {
            outer_margin = rnd.sample_outer_kernel_margin();
            modified = true;
        } else {
            rnd.skip_n(2);
        }

        if rnd.with_probability(config.inner_rad_mut_prob) {
            inner_circ = rnd.sample_circularity();
            modified = true;
        } else {
            rnd.skip_n(2);
        }

        if rnd.with_probability(config.outer_rad_mut_prob) {
            outer_circ = rnd.sample_circularity();
            modified = true;
        } else {
            rnd.skip_n(2);
        }

        outer_size = inner_size + outer_margin;

        // constraint 1: small circles must be fully rect
        if inner_size <= 3 {
            inner_circ = 0.0;
        }
        if outer_size <= 3 {
            outer_circ = 0.0;
        }

        // constraint 2: outer size cannot be smaller than inner
        assert!(outer_size >= inner_size); // this shoulnt happen -> crash!

        if modified {
            self.inner_kernel = Kernel::new(inner_size, inner_circ);
            self.outer_kernel = Kernel::new(outer_size, outer_circ);
        }
    }

    fn lock_previous_location(&mut self, delay: usize, map: &Map, gen_config: &GenerationConfig) {
        if self.position_history.len() <= delay {
            return; // history not long enough yet
        }

        // get position of 'delay' steps ago
        let previous_pos = &self.position_history[self.steps - delay];

        // TODO: use inner or outer? or just define by hand?
        let max_kernel_size = gen_config
            .inner_size_probs
            .values
            .as_ref()
            .unwrap()
            .iter()
            .max()
            .unwrap();

        // TODO: rework this by reusing functionality -> lock possible cells
        let offset: usize = *max_kernel_size; // offset of kernel wrt. position (top/left)
        let extend: usize = (max_kernel_size * 2) - offset; // how much kernel extends position (bot/right)
        let top_left = previous_pos
            .shifted_by(-offset.to_i32().unwrap(), -offset.to_i32().unwrap())
            .unwrap();
        let bot_right = previous_pos
            .shifted_by(extend.to_i32().unwrap(), extend.to_i32().unwrap())
            .unwrap();

        // check if operation valid
        if !map.pos_in_bounds(&top_left) || !map.pos_in_bounds(&bot_right) {
            return;
        }

        // lock all
        let mut view = self
            .locked_positions
            .slice_mut(s![top_left.x..=bot_right.x, top_left.y..=bot_right.y]);
        for lock_status in view.iter_mut() {
            *lock_status = true;
        }
    }
}
