extern crate clap;
extern crate dotenv;
extern crate pretty_env_logger;
#[macro_use]
extern crate log;
extern crate twitchchat;
extern crate local_oauth_client;
extern crate directories;
extern crate reqwest;
extern crate serde;
extern crate cursive;

use clap::{Arg, App};
use dotenv::dotenv;
use std::net::TcpStream;
use std::path::Path;
use twitchchat::{Client, Message, UserConfig, Writer};
use local_oauth_client::local_client;
use directories::ProjectDirs;
use serde::Deserialize;
use cursive::Cursive;
use cursive::views::{ViewRef, TextView, ScrollView, Dialog, LinearLayout, EditView};
use cursive::align::HAlign;
use cursive::traits::Identifiable;
use cursive::view::Scrollable;
use cursive::utils::markup::StyledString;
use cursive::theme::Effect;
use std::sync::mpsc::{Sender, Receiver};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const TWITCH_API_USERID: &str = include_str!("twitch.userid");
const TWITCH_API_SECRET: &str = include_str!("twitch.secret");
const TWITCH_AUTH_URL: &str = "https://id.twitch.tv/oauth2/authorize";
const TWITCH_TOKEN_URL: &str = "https://id.twitch.tv/oauth2/token";
const TWITCH_USER_URL: &str = "https://api.twitch.tv/helix/users";

#[derive(Debug, Deserialize)]
struct TwitchUserAPIUser {
    id: String,
    login: String,
    display_name: String,
    // following needs to be escaped with r# since type is reserved word in Rust
    r#type: String,
    broadcaster_type: String,
    description: String,
    profile_image_url: String,
    offline_image_url: String,
    view_count: u64,
    email: String
}

#[derive(Debug, Deserialize)]
struct TwitchUserAPIResponse {
    data: Vec<TwitchUserAPIUser>
}

struct TwitchWriter {
    my_nick: String,
    writer: Writer,
    channel: String
}

struct TwitchMessage {
    nick: String,
    message: String
}

fn main() {
    // initialize dot environment so we can pull arguments from env, env files,
    //  commandline or as hardcoded values in code
    dotenv().ok();
    // initialize logger
    pretty_env_logger::init();

    let project_dir = ProjectDirs::from("me", "bcow", env!("CARGO_PKG_NAME")).unwrap();
    let token_cache_path = Path::new(project_dir.cache_dir()).join("token");
    // parse command line
    let matches = App::new(env!("CARGO_PKG_NAME"))
        .version(env!("CARGO_PKG_VERSION"))
        .author(env!("CARGO_PKG_AUTHORS"))
        .about("Twitch chat and stream client")
        .arg(Arg::with_name("STREAMER_NICK")
            .help("Twitch streamer to access")
            .required(true)
            .index(1))
        .get_matches();

    let streamer_nick = matches.value_of("STREAMER_NICK").unwrap();

    // init oauth configuration with client specific variables
    let twitch_oauth_client = local_client::OAuth2Client::new(
        TWITCH_API_USERID,
        TWITCH_API_SECRET,
        TWITCH_AUTH_URL,
        TWITCH_TOKEN_URL)
        .add_scope("chat:read")
        .add_scope("chat:edit")
        .add_scope("user:read:email")
        .open_browser()
        .set_cache_path(token_cache_path).unwrap()
        .set_ok_message(include_str!("banner.html").to_string());

    info!("Begin authentication");
    let twitch_access_token = match twitch_oauth_client.get_access_token() {
            Ok(token) => {
                token.access_token
            },
            Err(error) => {
                panic!(error)
            }
        };

    // figure the tokens user info based on the bearer token
    let http_client = reqwest::Client::new();
    let user_api_resp: TwitchUserAPIResponse = http_client
        .get(TWITCH_USER_URL)
        .header("Authorization", format!("Bearer {}", twitch_access_token))
        .send().unwrap()
        .json().unwrap();
    debug!("{:?}", user_api_resp);
    let twitch_nick = user_api_resp.data[0].login.clone();
    info!("Twitch user '{}' authenticated succesfully.", twitch_nick);

    let (tx, rx): (Sender<TwitchWriter>, Receiver<TwitchWriter>) = mpsc::channel();
    let (tx1, rx1): (Sender<TwitchMessage>, Receiver<TwitchMessage>) = mpsc::channel();
    let matches_clone = matches.clone();
    thread::spawn(move || {
        // now we have the token we can actually create a chat instance with twitch
        // connect to twitch via a websocket stream, creating a read/write pair
        let (read, write) = {
            let stream = TcpStream::connect(twitchchat::TWITCH_IRC_ADDRESS).unwrap();
            // create synchronous 'adapters' for the tcpstream
            twitchchat::sync_adapters(stream.try_clone().unwrap(), stream)
        };

        // create a config
        let conf = UserConfig::builder()
            .nick(&user_api_resp.data[0].login)
            .token(format!("oauth:{}", twitch_access_token))
            .tags()
            .commands()
            .membership()
            .build().unwrap();

        // create a client from the read/write pair
        let mut client = Client::new(read, write);

        // register with the server, using the config
        client.register(conf).unwrap();

        // wait until the server tells us who we are
        client.wait_for_ready().unwrap();
        let w = client.writer();
        // join a channel
        let streamer_nick = matches_clone.value_of("STREAMER_NICK").unwrap();
        w.join(streamer_nick).unwrap();

        // send the writer to main thread
        let twitch_writer = TwitchWriter {
            my_nick: twitch_nick,
            writer: w,
            channel: streamer_nick.to_string()
        };
        tx.send(twitch_writer).unwrap();

        while let Ok(msg) = client.read_message() {
            // if its a user message on a channel
            if let Message::PrivMsg(msg) = msg {
                tx1.send(TwitchMessage { nick: msg.user().to_string(), message: msg.message().to_string() }).unwrap();
            }
        }
    });
    let writer: TwitchWriter;
    loop {
        let mut count: u8 = 0;
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(recv) => {
                writer = recv;
                break;
            },
            Err(_) => {
                count = count + 1;
                if count > 30 {
                    panic!("Did not receive Twitch chat writer object. Timeouted at 60 seconds.");
                }
            }
        }
    }

    // init tui toolkit for presenting views for the user
    let mut siv = Cursive::default();
    siv.set_user_data(writer);
    siv.add_fullscreen_layer(
        // sequence of scrollable and with_id is meaningful since below in the loop we look for the inner_mut for it
        Dialog::around(
            LinearLayout::vertical()
                .child(TextView::empty().scrollable().with_id("chatlog"))
                .child(EditView::new().on_submit(send).with_id("message")))
            .title(format!("#{}", streamer_nick))
            // This is the alignment for the button
            .h_align(HAlign::Right)
            .button("Quit", |s| s.quit())
    );

    loop {
        match rx1.try_recv() {
            Ok(msg) => {
                show(&mut siv, &msg);
            },
            Err(_) => ()
        }
        // update screen
        siv.step();
        siv.refresh();
        if !siv.is_running() {
            // if quit() activated we jump out here
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    info!("Glitchy is quitting.");
}

fn show(siv: &mut Cursive, msg: &TwitchMessage) {
    let mut view: ViewRef<ScrollView<TextView>> = siv.find_id("chatlog").unwrap();
    let twitch = siv.user_data::<TwitchWriter>().unwrap();
    let mention = format!("@{}", twitch.my_nick);
    let raw_text = format!("<{}> {}\n", msg.nick, msg.message);
    let mut styled_text: StyledString;
    if msg.message.contains(&mention) {
        // someone mentioned me, highlight
        styled_text = StyledString::styled(raw_text, Effect::Underline);
    } else if msg.nick == twitch.my_nick {
        // i sent a message, distinguish it
        styled_text = StyledString::styled(raw_text, Effect::Bold);
    } else {
        // just a normal message
        styled_text = StyledString::plain(raw_text);
    }
    view.get_inner_mut().append(styled_text);
    view.scroll_to_bottom();
}

fn send(siv: &mut Cursive, message: &str) {
    let twitch = siv.user_data::<TwitchWriter>().unwrap();
    let my_nick = twitch.my_nick.clone();
    if !message.is_empty() {
        twitch.writer.send(&twitch.channel, message).unwrap();
        show(siv, &TwitchMessage { nick: my_nick, message: message.to_string() });
    }

    // clear the input prompt
    let mut prompt: ViewRef<EditView> = siv.find_id("message").unwrap();
    prompt.set_content("");
}

// eof