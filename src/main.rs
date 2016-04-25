extern crate irc;
extern crate telegram_bot;
extern crate toml;
extern crate rustc_serialize;

use std::default::Default;
use std::thread::spawn;
use std::fs::File;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::collections::hash_map::{HashMap, Entry};
use irc::client::prelude::{IrcServer, ServerExt};
use rustc_serialize::Decodable;
use telegram_bot::{Api, ListeningMethod, ListeningAction};
use telegram_bot::types::{User, MessageType};

const CONFIG_FILE: &'static str = "config.toml";
const CHAT_IDS_FILE: &'static str = "chat_ids";

type ChatID = telegram_bot::types::Integer;
type IrcChannel = String;
type TelegramGroup = String;

#[derive(Clone, Default, Debug)]
struct RelayState {
    // Map from IRC channel to Telegram group
    tg_group: HashMap<IrcChannel, TelegramGroup>,
    // Map from Telegram group to IRC channel
    irc_channel: HashMap<TelegramGroup, IrcChannel>,
    // Map from Telegram group name to chat_id
    chat_ids: HashMap<TelegramGroup, ChatID>,
    config: Config,
}

#[derive(Clone, Default, RustcDecodable, Debug)]
struct Config {
    pub irc: irc::client::data::Config,
    pub token: String,
    pub maps: HashMap<TelegramGroup, IrcChannel>,
}

fn format_tg_nick(user: &User) -> String {
    match *user {
        User { first_name: ref first, last_name: None, .. } => format!("{}", first),
        User { first_name: ref first, last_name: Some(ref last), .. } => {
            format!("{} {}", first, last)
        }
    }
}

fn load_toml<T: Default + Decodable>(path: &str) -> T {
    let mut config_toml = String::new();
    let mut file = match File::open(&path) {
        Ok(file) => file,
        Err(_) => {
            println!("[WARN] Could not find file \"{}\", using default!", path);
            return T::default();
        }
    };
    file.read_to_string(&mut config_toml)
        .unwrap_or_else(|err| panic!("error while reading config: [{}]", err));
    let mut parser = toml::Parser::new(&config_toml);
    let toml = parser.parse();
    if toml.is_none() {
        for err in &parser.errors {
            let (loline, locol) = parser.to_linecol(err.lo);
            let (hiline, hicol) = parser.to_linecol(err.hi);
            println!("[ERR] {}:{}:{}-{}:{} error: {}",
                     path,
                     loline,
                     locol,
                     hiline,
                     hicol,
                     err.desc);
        }
    }

    let config = toml::Value::Table(toml.unwrap());
    match toml::decode(config) {
        Some(t) => t,
        None => panic!("Error while deserializing config"),
    }
}

fn load_config(path: &str) -> Config {
    let mut config: Config = load_toml(path);
    config.irc.channels = Some(config.maps.values().map(|v| v.clone()).collect());
    config
}

fn load_chat_ids(path: &str) -> HashMap<TelegramGroup, ChatID> {
    let mapping = load_toml(path);
    for (group, chat_id) in &mapping {
        println!("[INFO] Loaded Telegram group \"{}\" with id {}",
                 group,
                 chat_id);
    }
    mapping
}

fn save_chat_ids(path: &str, chat_ids: &HashMap<TelegramGroup, ChatID>) {
    let mut file = File::create(path).unwrap();
    file.write_all(toml::encode_str(&chat_ids).as_bytes()).unwrap();
}

fn handle_irc<T: ServerExt>(irc: T, tg: Arc<Api>, state: Arc<Mutex<RelayState>>) {
    let tg = tg.clone();
    for message in irc.iter() {
        match message {
            Ok(msg) => {
                // Acquire lock of shared state
                let state = state.lock().unwrap();

                // The following conditions must be met in order for a message to be relayed.
                // 1. We must be receiving a PRIVMSG
                // 2. The message must have been sent by some user
                // 2. The IRC channel in question must be present in the mapping
                // 3. The Telegram group associated with the channel must have a known group_id

                if let irc::client::data::Command::PRIVMSG(ref channel, ref t) = msg.command {
                    // 1. PRIVMSG received
                    if let Some(ref nick) = msg.source_nickname() {
                        // 2. Sender's nick exists
                        match state.tg_group.get(channel) {
                            Some(group) => {
                                // 3. IRC channel exists in the mapping
                                if let Some(id) = state.chat_ids.get(group) {
                                    // 4. Telegram group_id is known, relay the message
                                    let relay_msg = format!("<{nick}> {message}",
                                                            nick = nick,
                                                            message = t);
                                    println!("[INFO] Relaying \"{}\" → \"{}\": {}",
                                             channel,
                                             group,
                                             relay_msg);
                                    let _ = tg.send_message(*id, relay_msg, None, None, None, None);
                                } else {
                                    // Telegram group_id has not yet been seen
                                    println!("[WARN] Cannot find telegram group \"{}\"", group);
                                }
                            }
                            None => {
                                // IRC channel not specified in config
                            }
                        }
                    }
                }
            }
            Err(err) => {
                println!("IRC error: {}", err);
            }
        }
    }
}

fn handle_tg<T: ServerExt>(irc: T, tg: Arc<Api>, state: Arc<Mutex<RelayState>>) {
    let tg = tg.clone();
    let mut listener = tg.listener(ListeningMethod::LongPoll(None));

    // Fetch new updates via long poll method
    let _ = listener.listen(|u| {

        // Check for message in received update
        if let Some(m) = u.message {
            let mut state = state.lock().unwrap();

            // The following conditions must be met in order for a message to be relayed.
            // 1. We must be receiving a message from a group (handle channels in the future?)
            // 2. The Telegram group in question must be present in the mapping


            match m.chat {
                telegram_bot::types::Chat::Group { id, title, .. } => {

                    // Check if channel's id should be recorded
                    if state.chat_ids.get(&title).is_none() {
                        println!("[INFO] Found telegram group \"{}\" with id {}", title, id);
                        println!("[INFO] Saving to \"{}\"", CHAT_IDS_FILE);
                        state.chat_ids.insert(title.clone(), id);
                        save_chat_ids(CHAT_IDS_FILE, &state.chat_ids);
                    }


                    match state.irc_channel.entry(title.clone()) {
                        // Telegram channel exists in the mapping
                        Entry::Occupied(e) => {
                            let channel = e.get();

                            match m.msg {
                                MessageType::Text(t) => {
                                    // Print received text message to stdout
                                    let nick = format_tg_nick(&m.from);
                                    let relay_msg = format!("<{nick}> {message}",
                                                            nick = nick,
                                                            message = t);
                                    println!("[INFO] Relaying \"{}\" → \"{}\": {}",
                                             title,
                                             channel,
                                             relay_msg);
                                    irc.send_privmsg(channel, &relay_msg).unwrap();
                                }
                                _ => {}
                            }
                        }
                        Entry::Vacant(_) => {
                            // Telegram group not specified in config
                        }
                    }
                }
                _ => (),
            }
        }

        // If none of the "try!" statements returned an error: It's Ok!
        Ok(ListeningAction::Continue)
    });
}

fn main() {
    // Parse config file and chat IDs
    let config = load_config(CONFIG_FILE);
    let chat_ids = load_chat_ids(CHAT_IDS_FILE);

    // Initialize IRC connection and identify with server
    let irc_cfg = config.irc.clone();
    let client = IrcServer::from_config(irc_cfg).expect("Could not connect to server, check configuration.");
    client.send_sasl_plain().expect("Could not authenticate with SASL.");
    client.identify().expect("Could not identify to server.");

    // Initialize Telegram API and package into Arc
    let token = config.token.clone();
    let api = Api::from_token(&token).unwrap();
    let me = api.get_me().unwrap();
    let arc_tg = Arc::new(api);

    // Setup Telegram <-> IRC bridges
    let irc_channel = config.maps.clone();
    // Reverse the hashmap
    let tg_group = config.maps.iter().map(|(k, v)| (v.clone(), k.clone())).collect();

    // Initialize shared state
    let state = Arc::new(Mutex::new(RelayState {
        tg_group: tg_group,
        irc_channel: irc_channel,
        chat_ids: chat_ids,
        config: config,
    }));

    println!("[INFO] Telegram username: @{}", me.username.unwrap());
    println!("[INFO] IRC nick: {}", client.current_nickname());

    // Start threads handling irc and telegram
    let irc_handle = {
        let client = client.clone();
        let api = arc_tg.clone();
        let state = state.clone();
        spawn(move || handle_irc(client, api, state))
    };
    let tg_handle = {
        let client = client.clone();
        let api = arc_tg.clone();
        let state = state.clone();
        spawn(move || handle_tg(client, api, state))
    };

    // Clean up threads. This should probably never need to be run, as this would imply
    // that both functions returned, where in most cases the bot will either crash or
    // be killed from the command line.
    irc_handle.join().unwrap();
    tg_handle.join().unwrap();
    println!("[UNICORN] I don't think that this line should ever be printed.");
}
