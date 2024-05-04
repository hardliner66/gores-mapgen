use std::f32::consts::SQRT_2;

use std::collections::BTreeMap;

use crate::{
    config::GenerationConfig,
    config::GenerationConfigStorage,
    debug::DebugLayer,
    kernel::Kernel,
    map::{BlockType, Map},
    position::Position,
    random::{Random, Seed},
    walker::CuteWalker,
};

use dt::dt_bool;
use macroquad::color::colors;
use ndarray::{Array2, Ix2};

pub struct Generator {
    pub walker: CuteWalker,
    pub walker2: Option<CuteWalker>,
    pub map: Map,
    pub rnd: Random,
    pub rnd2: Random,
    pub debug_layers: BTreeMap<&'static str, DebugLayer>,
}

impl Generator {
    /// derive a initial generator state based on a GenerationConfig
    pub fn new(config: &GenerationConfig, seed: Seed) -> Generator {
        let spawn = Position::new(50, 250);
        let map = Map::new(300, 300, BlockType::Hookable, spawn.clone());
        let init_inner_kernel = Kernel::new(5, 0.0);
        let init_outer_kernel = Kernel::new(7, 0.0);
        let walker = CuteWalker::new(
            spawn.clone(),
            init_inner_kernel.clone(),
            init_outer_kernel.clone(),
            config,
        );

        let mut rnd = Random::new(seed, config);

        let configs = GenerationConfig::get_configs();
        let skip_config = configs.get("skips").unwrap();
        let walker2 = CuteWalker::new(spawn, init_inner_kernel, init_outer_kernel, skip_config);
        let rnd2 = Random::new(Seed::from_random(&mut rnd), skip_config);

        let debug_layers =
            BTreeMap::from([("edge_bugs", DebugLayer::new(false, colors::RED, &map))]);

        Generator {
            walker,
            walker2: Some(walker2),
            map,
            rnd,
            rnd2,
            debug_layers,
        }
    }

    pub fn step(&mut self, config: &GenerationConfig) -> Result<(), &'static str> {
        // check if walker has reached goal position
        if self.walker.is_goal_reached(&config.waypoint_reached_dist) == Some(true) {
            self.walker.next_waypoint();
        }
        if let Some(ref mut walker2) = &mut self.walker2 {
            if walker2.is_goal_reached(&config.waypoint_reached_dist) == Some(true) {
                walker2.next_waypoint();
            }
        }

        let configs = GenerationConfig::get_configs();
        let skip_config = configs.get("skips").unwrap();

        if !self.walker.finished {
            // validate config - TODO: add build flag which skips this?
            config.validate()?;

            // randomly mutate kernel
            self.walker.mutate_kernel(config, &mut self.rnd);

            if let Some(ref mut walker2) = &mut self.walker2 {
                walker2.mutate_kernel(skip_config, &mut self.rnd2);
            }
            // perform one step
            self.walker
                .probabilistic_step(&mut self.map, config, &mut self.rnd)?;

            if let Some(ref mut walker2) = &mut self.walker2 {
                let _ = walker2.probabilistic_step(&mut self.map, skip_config, &mut self.rnd2);
            }
            // handle platforms
            self.walker.check_platform(
                &mut self.map,
                config.platform_distance_bounds.0,
                config.platform_distance_bounds.1,
            )?;
        }

        Ok(())
    }

    /// Post processing step to fix all existing edge-bugs, as certain inner/outer kernel
    /// configurations do not ensure a min. 1-block freeze padding consistently.
    fn fix_edge_bugs(&mut self) -> Result<Array2<bool>, &'static str> {
        let mut edge_bug = Array2::from_elem((self.map.width, self.map.height), false);
        let width = self.map.width;
        let height = self.map.height;

        for x in 0..width {
            for y in 0..height {
                let value = &self.map.grid[[x, y]];
                if *value == BlockType::Empty {
                    for dx in 0..=2 {
                        for dy in 0..=2 {
                            if dx == 1 && dy == 1 {
                                continue;
                            }

                            let neighbor_x = (x + dx)
                                .checked_sub(1)
                                .ok_or("fix edge bug out of bounds")?;
                            let neighbor_y = (y + dy)
                                .checked_sub(1)
                                .ok_or("fix edge bug out of bounds")?;
                            if neighbor_x < width && neighbor_y < height {
                                let neighbor_value = &self.map.grid[[neighbor_x, neighbor_y]];
                                if *neighbor_value == BlockType::Hookable {
                                    edge_bug[[x, y]] = true;
                                    // break;
                                }
                            }
                        }
                    }

                    if edge_bug[[x, y]] {
                        self.map.grid[[x, y]] = BlockType::Freeze;
                    }
                }
            }
        }

        Ok(edge_bug)
    }

    /// Using a distance transform this function will fill up all empty blocks that are too far
    /// from the next solid/non-empty block
    pub fn fill_area(&mut self, max_distance: &f32) -> Array2<f32> {
        let grid = self.map.grid.map(|val| *val != BlockType::Empty);

        // euclidean distance transform
        let distance = dt_bool::<f32>(&grid.into_dyn())
            .into_dimensionality::<Ix2>()
            .unwrap();

        self.map
            .grid
            .zip_mut_with(&distance, |block_type, distance| {
                // only modify empty blocks
                if *block_type != BlockType::Empty {
                    return;
                }

                if *distance > *max_distance + SQRT_2 {
                    *block_type = BlockType::Hookable;
                } else if *distance > *max_distance {
                    *block_type = BlockType::Freeze;
                }
            });

        distance
    }

    pub fn post_processing(&mut self, config: &GenerationConfig) {
        let edge_bugs = self.fix_edge_bugs().expect("fix edge bugs failed");
        self.map
            .generate_room(&self.map.spawn.clone(), 4, 3, Some(&BlockType::Start))
            .expect("start room generation failed");
        self.map
            .generate_room(&self.walker.pos.clone(), 4, 3, Some(&BlockType::Finish))
            .expect("start finish room generation");

        self.fill_area(&config.max_distance);

        // set debug layers
        self.debug_layers.get_mut("edge_bugs").unwrap().grid = edge_bugs;
    }

    /// Generates an entire map with a single function call. This function is used by the CLI.
    /// It is important to keep this function up to date with the editor generation, so that
    /// fixed seed map generations result in the same map.
    pub fn generate_map(
        max_steps: usize,
        seed: &Seed,
        config: &GenerationConfig,
    ) -> Result<Map, &'static str> {
        let mut gen = Generator::new(config, seed.clone());

        for _ in 0..max_steps {
            if gen.walker.finished {
                break;
            }
            gen.step(config)?;
        }

        gen.post_processing(config);

        Ok(gen.map)
    }
}
