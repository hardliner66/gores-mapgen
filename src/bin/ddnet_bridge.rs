use clap::Parser;
use core::net::{IpAddr, Ipv4Addr, SocketAddr};
use gores_mapgen_rust::config::MapConfig;
use gores_mapgen_rust::random::Seed;
use gores_mapgen_rust::{config::GenerationConfig, generator::Generator};
use std::collections::HashMap;

use regex::Regex;
use std::{path::PathBuf, process::exit, str::FromStr, time::Duration};
use telnet::{Event, Telnet};

#[derive(Parser, Debug)]
#[command(name = "DDNet Bridge")]
#[command(version = "0.1a")]
#[command(about = "Detect DDNet-Server votes via econ to trigger map generations", long_about = None)]
enum Command {
    #[clap(name = "start", about = "start the ddnet bridge")]
    StartBridge(BridgeArgs),

    #[clap(
        name = "presets",
        about = "print a list of available generation configs"
    )]
    ListPresets,
}

#[derive(Parser, Debug)]
struct BridgeArgs {
    /// ec_password
    econ_pass: String,

    /// ec_port
    econ_port: u16,

    /// telnet buffer size (amount of bytes/chars)
    #[arg(default_value_t = 256, long, short('b'))]
    telnet_buffer: usize,

    /// debug to console
    #[arg(short, long, default_value_t = false)]
    debug: bool,

    /// path to maps folder
    maps: PathBuf,
}

/// keeps track of the server bridge state
struct ServerBridge {
    /// econ connection to game server
    econ: Econ,

    /// stores information about vote while its still pending
    pending_vote: Option<Vote>,

    /// stores all available map generation configs
    gen_configs: HashMap<String, GenerationConfig>,

    /// stores start arguments
    args: BridgeArgs,
}

impl ServerBridge {
    fn new(args: BridgeArgs) -> ServerBridge {
        ServerBridge {
            econ: Econ::new(args.econ_port, args.telnet_buffer),
            pending_vote: None,
            gen_configs: GenerationConfig::get_all_configs(),
            args,
        }
    }

    fn start(&mut self) {
        loop {
            if let Some(data) = self.econ.read() {
                if self.args.debug {
                    println!("[RECV DEBUG]: {:?}", data);
                }

                if !self.econ.authed {
                    self.check_auth(&data);
                } else {
                    self.check_vote(&data);
                }
            }
        }
    }

    /// checks whether the econ message regards votes, if yes return a Vote struct
    pub fn check_vote(&mut self, data: &String) {
        // this regex detects all possible chat messages involving votes
        let vote_regex = Regex::new(r"(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}) I chat: \*\*\* (Vote passed|Vote failed|'(.+?)' called .+ option '(.+?)' \((.+?)\))\n").unwrap();
        let result = vote_regex.captures_iter(&data);

        for mat in result {
            let _date = mat.get(1).unwrap();
            let message = mat.get(2);

            // determine vote event type
            if let Some(message) = message.map(|v| v.as_str()) {
                match message {
                    "Vote passed" => {
                        println!("[VOTE]: Success");
                        self.handle_vote();
                    }
                    "Vote failed" => {
                        self.pending_vote = None;
                        println!("[VOTE]: Failed");
                    }
                    // vote started messages begin with 'player_name'
                    _ if message.starts_with('\'') => {
                        let player_name = mat.get(3).unwrap().as_str().to_string();
                        let vote_name = mat.get(4).unwrap().as_str().to_string();
                        let vote_reason = mat.get(5).unwrap().as_str().to_string();

                        println!(
                            "[VOTE]: vote_name={}, vote_reason={}, player={}",
                            &vote_name, &vote_reason, &player_name
                        );

                        self.pending_vote = Some(Vote {
                            _player_name: player_name,
                            vote_name,
                            vote_reason,
                        });
                    }
                    // panic if for some holy reason something else matched the regex
                    _ => panic!(),
                }
            }
        }
    }

    /// checks whether the econ message regards authentication
    pub fn check_auth(&mut self, data: &String) {
        if data == "Enter password:\n" {
            self.econ.send_rcon_cmd(self.args.econ_pass.clone());
            println!("[AUTH] Sending login");
        } else if data.starts_with("Authentication successful") {
            println!("[AUTH] Success");
            println!("[GEN] Generating initial map");
            self.econ.authed = true;
            self.generate_and_change_map(
                &Seed::from_u64(1337),
                &GenerationConfig::get_initial_config(),
                &MapConfig::get_initial_config(),
            );
        } else if data.starts_with("Wrong password") {
            println!("[AUTH] Wrong Password!");
            std::process::exit(1);
        }
    }

    pub fn handle_vote(&mut self) {
        let vote = std::mem::replace(&mut self.pending_vote, None).expect("no pending vote");

        if vote.vote_name.starts_with("generate") {
            // derive Seed from vote reason
            let seed = if vote.vote_reason == "No reason given" {
                Seed::random()
            } else if let Ok(seed_u64) = vote.vote_reason.parse::<u64>() {
                Seed::from_u64(seed_u64)
            } else {
                Seed::from_string(&vote.vote_reason)
            };

            // split vote name to get selected preset
            let mut vote_parts = vote.vote_name.split_whitespace();
            let vote_type = vote_parts.next().expect("should have exactly two parts");
            assert_eq!(vote_type, "generate");
            let vote_preset = vote_parts.next().expect("should have exactly two parts");
            assert!(vote_parts.next().is_none(), "should have exactly two parts");

            // get config based on preset name
            let gen_config = self
                .gen_configs
                .get(vote_preset)
                .expect("preset does not exist!")
                .clone();

            self.generate_and_change_map(&seed, &gen_config, &MapConfig::get_initial_config());
        }
    }

    fn generate_and_change_map(
        &mut self,
        seed: &Seed,
        gen_config: &GenerationConfig,
        map_config: &MapConfig,
    ) {
        println!("[GEN] Starting Map Generation!");
        self.econ
            .send_rcon_cmd(format!("say [GEN] Generating Map, seed={:?}", &seed));
        let map_path = self
            .args
            .maps
            .canonicalize()
            .unwrap()
            .join("random_map.map");
        match Generator::generate_map(30_000, seed, gen_config, map_config) {
            Ok(map) => {
                println!("[GEN] Finished Map Generation!");
                map.export(&map_path);
                println!("[GEN] Map was exported");
                self.econ.send_rcon_cmd("change_map random_map".to_string());
                self.econ.send_rcon_cmd("reload".to_string());
                self.econ.send_rcon_cmd("say [GEN] Done...".to_string());
            }
            Err(err) => {
                println!("[GEN] Generation Error: {:?}", err);
                self.econ
                    .send_rcon_cmd(format!("say [GEN] Failed due to: {:}", err));
                self.econ.send_rcon_cmd("say just try again :)".to_string());
            }
        }
    }
}

#[derive(Debug)]
struct Vote {
    _player_name: String,
    vote_name: String,
    vote_reason: String,
}

struct Econ {
    telnet: Telnet,
    authed: bool,
}

impl Econ {
    pub fn new(port: u16, buffer_size: usize) -> Econ {
        let address = SocketAddr::new(
            IpAddr::V4(Ipv4Addr::from_str("127.0.0.1").expect("Invalid address")),
            port,
        );

        Econ {
            telnet: Telnet::connect_timeout(&address, buffer_size, Duration::from_secs(10))
                .unwrap_or_else(|err| {
                    println!("Coulnt establish telnet connection\nError: {:?}", err);
                    exit(1);
                }),
            authed: false,
        }
    }

    pub fn read(&mut self) -> Option<String> {
        let event = self.telnet.read().expect("telnet read error");

        if let Event::Data(buffer) = event {
            Some(String::from_utf8_lossy(&buffer).replace('\0', ""))
        } else {
            None
        }
    }

    pub fn send_rcon_cmd(&mut self, mut command: String) {
        command.push('\n');
        self.telnet
            .write(command.as_bytes())
            .expect("telnet write error");
    }
}

fn list_presets() {
    let configs = GenerationConfig::get_all_configs();
    for preset in configs.keys() {
        println!("{}", preset);
    }
}

fn main() {
    match Command::parse() {
        Command::StartBridge(bridge_args) => {
            let mut bridge = ServerBridge::new(bridge_args);
            bridge.start();
        }
        Command::ListPresets => list_presets(),
    }
}
