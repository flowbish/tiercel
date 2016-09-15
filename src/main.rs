extern crate irc;
extern crate telegram_bot;
extern crate toml;
extern crate hyper;
extern crate rustc_serialize;
extern crate regex;
#[macro_use] extern crate lazy_static;

mod types;
mod error;

use std::default::Default;
use std::thread;
use std::time::Duration;
use std::fs::File;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::collections::hash_map::{HashMap, Entry};
use std::path::{Path,PathBuf};
use std::fs;
use irc::client::prelude::{IrcServer, ServerExt};
use rustc_serialize::Decodable;
use hyper::Url;
use regex::Regex;
use telegram_bot::{Api, ListeningMethod, ListeningAction};
use telegram_bot::types::{User, MessageType};

use types::{Config, RelayState, ChatID, TelegramGroup, TGFile};
use types::download_file_user;

const CONFIG_FILE: &'static str = "config.toml";
const CHAT_IDS_FILE: &'static str = "chat_ids";

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

fn ensure_dir(path: &Path) {
    let _ = fs::create_dir(&path);
}

fn save_chat_ids(path: &str, chat_ids: &HashMap<TelegramGroup, ChatID>) {
    let mut file = File::create(path).unwrap();
    file.write_all(toml::encode_str(&chat_ids).as_bytes()).unwrap();
}

fn handle_irc<T: ServerExt>(irc: T, tg: Arc<Api>, config: Config, state: Arc<Mutex<RelayState>>) {
    let tg = tg.clone();
    for message in irc.iter() {
        match message {
            Ok(msg) => {
                // Acquire lock of shared state
                let state = state.lock().unwrap();

                // Debug print any messages from server
                if config.debug.unwrap_or(false) {
                    println!("[DEBUG] {}", msg.to_string());
                }

                // The following conditions must be met in order for a message to be relayed.
                // 1. We must be receiving a PRIVMSG
                // 2. The message must have been sent by some user
                // 2. The IRC channel in question must be present in the mapping
                // 3. The Telegram group associated with the channel must have a known group_id

                // 1. PRIVMSG received
                if let irc::client::data::Command::PRIVMSG(ref channel, ref t) = msg.command {
                    // 2. Sender's nick exists
                    if let Some(ref nick) = msg.source_nickname() {
                        match state.tg_group.get(channel) {
                            // 3. IRC channel exists in the mapping
                            Some(group) => {
                                // 4. Telegram group_id is known, relay the message
                                if let Some(id) = state.chat_ids.get(group) {
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
                println!("[ERROR] IRC error: {}", err);
                thread::sleep(Duration::new(10, 0));
            }
        }
    }
}

fn handle_tg<T: ServerExt>(irc: T, tg: Arc<Api>, config: Config, state: Arc<Mutex<RelayState>>) {
    let tg = tg.clone();
    let mut listener = tg.listener(ListeningMethod::LongPoll(None));

    loop {
        // Fetch new updates via long poll method
        let res = listener.listen(|u| {

            // Check for message in received update
            if let Some(m) = u.message {
                let mut state = state.lock().unwrap();

                // Debug print any messages from server
                if config.debug.unwrap_or(false) {
                    println!("[DEBUG] {:?}", m);
                }

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



                        if let Entry::Occupied(e) = state.irc_channel.entry(title.clone()){
                            let channel = e.get();
                            let nick = format_tg_nick(&m.from);
                            let mut message = String::new();

                            if config.relay_media.unwrap_or(false) {
                                if let (&Some(ref base_url), &Some(ref download_dir)) = (&config.base_url, &config.download_dir) {
                                    if let Some(tgfile) = TGFile::from_message(m.msg.clone()) {
                                        if let Ok(file) = tg.get_file(&tgfile.file_id()) {
                                            if let Some(path) = file.file_path {
                                                let tg_url = Url::parse(&tg.get_file_url(&path)).unwrap();
                                                let client_url = download_file_user(&tg_url, &m.from, &Path::new(&download_dir), &base_url).unwrap();
                                                message.push_str(&client_url.serialize());
                                            }
                                        }
                                    }
                                }
                            }

                            match m.msg {
                                MessageType::Text(t) => {
                                    message.push_str(&t);
                                },
                                MessageType::Sticker(sticker) => {
                                    let sticker_msg = if let Some(emoji) = sticker.emoji {
                                        format!("(Sticker) {}", emoji)
                                    } else {
                                        "(Sticker)".into()
                                    };
                                    message.push_str(&sticker_msg);
                                }
                                _ => {}
                            }

                            // Handle replies
                            if let Some(msg) = m.reply {
                                if tg.get_me().map(|u| u == msg.from).unwrap_or(false) {
                                    if let MessageType::Text(t) = msg.msg {
                                        lazy_static! {
                                            static ref RE: Regex = Regex::new("^<([^>]+)>").unwrap();
                                        }
                                        for username in RE.captures(&t) {
                                            if let Some(username) = username.at(1) {
                                                message = format!("{}: {}", username, message);
                                            }
                                        }
                                    }
                                } else {
                                    message = format!("{}: {}", format_tg_nick(&msg.from), message);
                                }
                            }

                            // Relay the message
                            let relay_msg = format!("<{nick}> {message}",
                                                    nick = nick,
                                                    message = message);
                            println!("[INFO] Relaying \"{}\" → \"{}\": {}",
                                     title,
                                     channel,
                                     relay_msg);
                            irc.send_privmsg(channel, &relay_msg).unwrap();
                        }
                    }
                    _ => (),
                }
            }

            // If none of the "try!" statements returned an error: It's Ok!
            Ok(ListeningAction::Continue)
        });
        if let Err(err) = res {
            println!("[ERROR] Telegram error: {}", err);
            thread::sleep(Duration::new(10, 0));
        }
    }
}

fn main() {
    // Parse config file and chat IDs
    let config = load_config(CONFIG_FILE);
    let chat_ids = load_chat_ids(CHAT_IDS_FILE);
    // Ensure that download dir exists
    if let Some(ref download_dir) = config.download_dir {
        ensure_dir(&PathBuf::from(download_dir));
    }

    // Initialize IRC connection and identify with server
    let irc_cfg = config.irc.clone();
    let client = IrcServer::from_config(irc_cfg).expect("Could not connect to server, check configuration.");
    if config.irc.password.is_some() {
        client.send_sasl_plain().expect("Could not authenticate with SASL.");
    }
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
    let state = Arc::new(Mutex::new(RelayState::new(tg_group, irc_channel, chat_ids)));

    println!("[INFO] Telegram username: @{}", me.username.unwrap());
    println!("[INFO] IRC nick: {}", client.current_nickname());

    // Wait for a little bit because IRC sucks?
    thread::sleep(Duration::new(3, 0));

    // Start threads handling irc and telegram
    let irc_handle = {
        let client = client.clone();
        let api = arc_tg.clone();
        let config = config.clone();
        let state = state.clone();
        thread::spawn(move || handle_irc(client, api, config, state))
    };
    let tg_handle = {
        let client = client.clone();
        let api = arc_tg.clone();
        let config = config.clone();
        let state = state.clone();
        thread::spawn(move || handle_tg(client, api, config, state))
    };

    // Clean up threads. This should probably never need to be run, as this would imply
    // that both functions returned, where in most cases the bot will either crash or
    // be killed from the command line.
    irc_handle.join().unwrap();
    tg_handle.join().unwrap();
    println!("[UNICORN] I don't think that this line should ever be printed.");
}
