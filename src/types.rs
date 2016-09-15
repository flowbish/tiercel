extern crate rand;

use self::rand::{Rng, thread_rng};
use std::path::{Path,PathBuf};
use std::fs::File;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io;
use hyper::Url;
use hyper::method::Method;
use hyper::client::Request;
use telegram_bot::types::{User, MessageType, Integer};
use error::Result;

pub type ChatID = Integer;
pub type IrcChannel = String;
pub type TelegramGroup = String;

#[derive(Clone)]
pub struct RelayState {
    // Map from IRC channel to Telegram group
    pub tg_group: HashMap<IrcChannel, TelegramGroup>,
    // Map from Telegram group to IRC channel
    pub irc_channel: HashMap<TelegramGroup, IrcChannel>,
    // Map from Telegram group name to chat_id
    pub chat_ids: HashMap<TelegramGroup, ChatID>,
    // Queue for messages in the Telegram -> Irc direction
    irc_message_queue: VecDeque<String>,
}

impl RelayState {
    pub fn new(tg_group: HashMap<IrcChannel, TelegramGroup>,
            irc_channel: HashMap<TelegramGroup, IrcChannel>,
            chat_ids: HashMap<TelegramGroup, ChatID>) -> RelayState {
        RelayState {
            tg_group: tg_group,
            irc_channel: irc_channel,
            chat_ids: chat_ids,
            irc_message_queue: VecDeque::new(),
        }
    }

    pub fn send_message_irc(&mut self, msg: String) {
        self.irc_message_queue.push_back(msg)
    }
}

/*
pub struct TgApi(Arc<::telelgram_bot::Api>);

pub impl TgApi {
    pub fn send_message(config: &Config, nick: &str, message: &str) -> Result<_> {
        let relay_msg = format!("<{nick}> {message}",
                                nick = nick,
                                message = t);
        tg.send_message(*id, relay_msg, None, None, None, None)
    }
}
*/

#[derive(Clone, Default, RustcDecodable, Debug)]
pub struct Config {
    pub irc: ::irc::client::data::Config,
    pub token: String,
    pub maps: HashMap<TelegramGroup, IrcChannel>,
    pub debug: Option<bool>,
    pub relay_media: Option<bool>,
    pub base_url: Option<Url>,
    pub download_dir: Option<String>,
}

pub struct TGFile {
    file_id: String,
    file_size: Integer
}

impl TGFile {
    pub fn from_message(msg: MessageType) -> Option<TGFile> {
        if let Some((file_id, Some(file_size))) = file_id_size(msg)  {
            Some(TGFile{ file_id: file_id, file_size: file_size})
        }
        else {
            None
        }
    }

    pub fn file_id(&self) -> &str {
        &self.file_id
    }

    pub fn file_size(&self) -> Integer {
        self.file_size
    }
}

fn file_id_size(msg: MessageType) -> Option<(String, Option<Integer>)> {
    match msg.clone() {
        MessageType::Photo(photos) => {
            let largest_photo = photos.last().unwrap();
            Some((largest_photo.file_id.clone(), largest_photo.file_size))
        },
        // MessageType::Sticker(sticker) => Some((sticker.file_id, sticker.file_size)),
        MessageType::Document(document) => Some((document.file_id, document.file_size)),
        MessageType::Audio(audio) => Some((audio.file_id, audio.file_size)),
        MessageType::Video(video) => Some((video.file_id, video.file_size)),
        MessageType::Voice(voice) => Some((voice.file_id, voice.file_size)),
        _ => None
    }
}

pub  fn download_file_user(url: &Url, user: &User, base_download_dir: &Path, base_url: &Url) -> Result<Url> {
    // Create the final download directory by combining the base
    // directory with the username, and ensure it exists.
    let base_user_path = user_path(&user, base_download_dir);
    ensure_dir(&base_user_path);

    // Create the final URL by combining the base URL and the
    // username.
    let base_user_url = user_url(&user, &base_url);

    download_file(&url, &base_user_path, &base_user_url)
}

fn generate_name() -> String {
    let mut rng = thread_rng();
    rng.gen_ascii_chars().take(6).collect()
}

fn replace_filename(filename: &str, name: &str) -> String {
    match filename.split('.').last() {
        Some(ext) => format!("{}.{}", name, ext),
        None => name.into()
    }
}

fn download_to_file(url: &Url, destination: &Path) -> Result<()>{
    // Create a request to download the file
    let req = try!(Request::new(Method::Get, url.clone()));
    let req = try!(req.start());
    let mut resp = try!(req.send());

    // Open file and copy downloaded data
    let mut file = try!(File::create(destination));
    try!(io::copy(&mut resp, &mut file));

    Ok(())
}

fn download_file(url: &Url, destination: &Path, baseurl: &Url) -> Result<Url> {
    // Grab the last portion of the url
    let filename = url.path().unwrap().last().unwrap();

    // Create path by combining filename from url with download dir
    let hash = generate_name();
    let filename = replace_filename(&filename, &hash);
    let mut path = destination.to_path_buf();
    path.push(filename.clone());
    path.set_file_name(&filename);

    try!(download_to_file(&url, &path));

    // Create the return url that maps to this filename
    let returl = push_url(baseurl.clone(), filename);
    Ok(returl)
}

fn ensure_dir(path: &Path) {
    let _ = fs::create_dir(&path);
}

fn user_path(user: &User, path: &Path) -> PathBuf {
    let mut user_path = path.to_path_buf();
    user_path.push(get_username(user));
    user_path
}

fn push_url(url: Url, item: String) -> Url {
    let mut url = url;
    url.path_mut().unwrap().push(item);
    url
}

fn user_url(user: &User, base_url: &Url) -> Url {
    push_url(base_url.clone(), get_username(&user))
}

fn get_username(user: &User) -> String {
    match user.username {
        Some(ref name) => name.clone(),
        None => "anonymous".into()
    }
}
