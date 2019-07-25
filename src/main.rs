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

use clap::{Arg, App};
use dotenv::dotenv;
use std::net::TcpStream;
use std::path::Path;
use twitchchat::{Client, Message, UserConfig};
use local_oauth_client::local_client;
use directories::ProjectDirs;
use serde::Deserialize;

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
    info!("Twitch user '{}' authenticated succesfully.", user_api_resp.data[0].login);

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
    let streamer_nick = matches.value_of("STREAMER_NICK").unwrap();
    info!("Joining to streamers chat #{}", streamer_nick);
    w.join(streamer_nick).unwrap();

    // read until the connection ends
    while let Ok(msg) = client.read_message() {
        // if its a user message on a channel
        if let Message::PrivMsg(msg) = msg {
            println!("<{}> {}", msg.user(), msg.message());
        }
    }
}
