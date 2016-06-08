extern crate irc;
extern crate telegram_bot;
extern crate toml;
extern crate hyper;
extern crate rustc_serialize;

use std::default::Default;
use std::thread;
use std::time::Duration;
use std::fs::File;
use std::io;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::collections::hash_map::{HashMap, Entry};
use std::path::{Path,PathBuf};
use irc::client::prelude::{IrcServer, ServerExt};
use rustc_serialize::Decodable;
use hyper::Url;
use hyper::method::Method;
use hyper::client::{Request};
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
}

#[derive(Clone, Default, RustcDecodable, Debug)]
struct Config {
    pub irc: irc::client::data::Config,
    pub token: String,
    pub maps: HashMap<TelegramGroup, IrcChannel>,
    pub debug: Option<bool>,
    pub relay_media: Option<bool>,
    pub base_url: Option<Url>,
    pub download_dir: Option<String>,
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

fn download_file(url: &Url, destination: &Path, baseurl: &Url) -> io::Result<Url> {
    // Create a request to download the file
    let req = Request::new(Method::Get, url.clone()).unwrap();
    let mut resp = req.start().unwrap().send().unwrap();

    // Grab the last portion of the url
    let filename = url.path().unwrap().last().unwrap();

    // Create path by combining filename from url with download dir
    let mut path = destination.to_path_buf();
    path.push(filename);

    // Open file and copy downloaded data
    let mut file = try!(File::create(path));
    std::io::copy(&mut resp, &mut file).unwrap();

    // Create the return url that maps to this filename
    let mut returl = baseurl.clone();
    returl.path_mut().unwrap().push(filename.clone());
    Ok(returl)
}

fn ensure_dir(path: &Path) {
    let _ = std::fs::create_dir(&path);
}

fn user_path(user: &User) -> String {
    match user.username {
        Some(ref name) => name.clone(),
        None => "anonymous".into()
    }
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
                println!("[ERROR] IRC error: {}", err);
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

                            match m.msg {
                                MessageType::Text(t) => {
                                    // Print received text message to stdout
                                    let relay_msg = format!("<{nick}> {message}",
                                                            nick = nick,
                                                            message = t);
                                    println!("[INFO] Relaying \"{}\" → \"{}\": {}",
                                            title,
                                            channel,
                                            relay_msg);
                                    irc.send_privmsg(channel, &relay_msg).unwrap();
                                },
                                MessageType::Photo(ps) => {
                                    // Print received text message to stdout
                                    if config.relay_media.unwrap_or(false) {
                                        if let Some(file) = ps.last() {
                                            let file = tg.get_file(&file.file_id).unwrap();
                                            if let Some(path) = file.file_path {
                                                let download_dir = PathBuf::from(config.download_dir.clone().unwrap());
                                                let mut base_url = config.base_url.clone().unwrap();

                                                // Create the final download directory by combining the base
                                                // directory with the username, and ensure it exists.
                                                let user_path = user_path(&m.from);
                                                let download_dir_user = download_dir.join(&user_path);
                                                ensure_dir(&download_dir_user);

                                                // Create the final URL by combining the base URL and the
                                                // username.
                                                base_url.path_mut().unwrap().push(user_path);
                                                let tg_url = Url::parse(&tg.get_file_url(&path)).unwrap();
                                                let local_url = download_file(&tg_url, &download_dir_user, &base_url).unwrap();

                                                // Send message to IRC
                                                let relay_msg = format!("<{nick}> {message}",
                                                                        nick = nick,
                                                                        message = local_url);
                                                println!("[INFO] Relaying \"{}\" → \"{}\": {}",
                                                        title,
                                                        channel,
                                                        relay_msg);
                                                irc.send_privmsg(channel, &relay_msg).unwrap();
                                            }
                                        }
                                    }
                                },
                                _ => {}
                            }
                        }
                    }
                    _ => (),
                }
            }

            // If none of the "try!" statements returned an error: It's Ok!
            Ok(ListeningAction::Continue)
        });
        if let Err(e) = res {
            println!("{}", e);
            std::process::exit(1);
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
    let state = Arc::new(Mutex::new(RelayState {
        tg_group: tg_group,
        irc_channel: irc_channel,
        chat_ids: chat_ids,
    }));

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
