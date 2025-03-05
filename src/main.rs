use std::collections::{BTreeSet, HashSet};
use std::fs::OpenOptions;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use atom_syndication::Feed;
use clap::{Parser, ValueEnum};
use rayon::prelude::*;
use reqwest::Url;
use rss::Channel;

mod error;
pub(crate) use error::{Error, ErrorKind};

mod walker;

enum RssOrAtomFeed {
    Rss2(Channel),
    Atom(Feed),
}

trait LinkProduceable {
    fn get_links(&self) -> Vec<Url>;
}

impl LinkProduceable for rss::Channel {
    fn get_links(&self) -> Vec<Url> {
        self.items()
            .iter()
            .filter_map(|item| item.link())
            .filter_map(|link| Url::parse(link).ok())
            .collect()
    }
}

impl LinkProduceable for atom_syndication::Feed {
    fn get_links(&self) -> Vec<Url> {
        self.entries()
            .iter()
            .flat_map(|entry| entry.links())
            .filter_map(|link| Url::parse(link.href()).ok())
            .collect()
    }
}

impl LinkProduceable for RssOrAtomFeed {
    fn get_links(&self) -> Vec<Url> {
        match self {
            RssOrAtomFeed::Rss2(channel) => channel.get_links(),
            RssOrAtomFeed::Atom(feed) => feed.get_links(),
        }
    }
}

trait FeedCacheReadable {
    fn read_cache(&self, feed_name: &str) -> Result<RssOrAtomFeed, Error>;
}

impl<F> FeedCacheReadable for F
where
    F: Fn(&str) -> Result<RssOrAtomFeed, Error>,
{
    fn read_cache(&self, feed_name: &str) -> Result<RssOrAtomFeed, Error> {
        (self)(feed_name)
    }
}

trait FeedGettable {
    fn get_feed(&self, feed_name: &str, url: &Url) -> Result<RssOrAtomFeed, Error>;
}

impl<F> FeedGettable for F
where
    F: Fn(&str, &Url) -> Result<RssOrAtomFeed, Error>,
{
    fn get_feed(&self, feed_name: &str, url: &Url) -> Result<RssOrAtomFeed, Error> {
        (self)(feed_name, url)
    }
}

trait FeedCacheWriteable {
    fn write_cache(&self, feed_name: &str, feed: &RssOrAtomFeed) -> Result<(), Error>;
}

impl<F> FeedCacheWriteable for F
where
    F: Fn(&str, &RssOrAtomFeed) -> Result<(), Error>,
{
    fn write_cache(&self, feed_name: &str, feed: &RssOrAtomFeed) -> Result<(), Error> {
        (self)(feed_name, feed)
    }
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
enum LogLevelArg {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl From<LogLevelArg> for log::LevelFilter {
    fn from(value: LogLevelArg) -> Self {
        match value {
            LogLevelArg::Off => Self::Off,
            LogLevelArg::Error => Self::Error,
            LogLevelArg::Warn => Self::Warn,
            LogLevelArg::Info => Self::Info,
            LogLevelArg::Debug => Self::Debug,
            LogLevelArg::Trace => Self::Trace,
        }
    }
}

fn get_feed_with_blocking_http_request(feed_name: &str, url: &Url) -> Result<RssOrAtomFeed, Error> {
    let resp = reqwest::blocking::get(url.as_str()).map_err(|err| {
        Error::new(ErrorKind::ReqwestErr(err)).with_data(format!("feed[{}]", feed_name))
    })?;

    let contents = resp.text().map_err(|err| {
        Error::new(ErrorKind::ReqwestErr(err)).with_data(format!("feed[{}]", feed_name))
    })?;

    let maybe_channel =
        Channel::read_from(contents.as_bytes()).map_err(|err| Error::new(ErrorKind::RssErr(err)));
    let maybe_feed = Feed::read_from(contents.as_bytes())
        .map_err(|err| Error::new(ErrorKind::AtomErr(err.to_string())));

    match (maybe_channel, maybe_feed) {
        (Ok(_), Ok(_)) => unreachable!(),
        (Ok(channel), Err(_)) => Ok(RssOrAtomFeed::Rss2(channel)),
        (Err(_), Ok(feed)) => Ok(RssOrAtomFeed::Atom(feed)),
        (Err(_), Err(_)) => Err(Error::new(ErrorKind::FeedIsNeitherAtomOrRss(
            feed_name.to_string(),
        ))),
    }
}

fn load_cached_feed_from_disk(cache_path: &Path) -> impl Fn(&str) -> Result<RssOrAtomFeed, Error> {
    let cache_path = cache_path.to_owned();

    move |feed_name: &str| {
        let cache_file_path = cache_path.join(feed_name);
        let cache_file = OpenOptions::new()
            .read(true)
            .open(&cache_file_path)
            .map_err(|err| {
                Error::new(ErrorKind::IoErr(err)).with_data(format!("feed[{}]", feed_name))
            })?;

        let channel_load_result = Channel::read_from(BufReader::new(cache_file))
            .map_err(|err| Error::new(ErrorKind::RssErr(err)));

        let cache_file = OpenOptions::new()
            .read(true)
            .open(&cache_file_path)
            .map_err(|err| {
                Error::new(ErrorKind::IoErr(err)).with_data(format!("feed[{}]", feed_name))
            })?;
        let feed_load_result = Feed::read_from(BufReader::new(cache_file))
            .map_err(|err| Error::new(ErrorKind::AtomErr(err.to_string())));

        match (channel_load_result, feed_load_result) {
            (Ok(_), Ok(_)) => unreachable!(),
            (Ok(channel), Err(_)) => Ok(RssOrAtomFeed::Rss2(channel)),
            (Err(_), Ok(feed)) => Ok(RssOrAtomFeed::Atom(feed)),
            (Err(_), Err(_)) => Err(Error::new(ErrorKind::InvalidCache(feed_name.to_string()))),
        }
    }
}

fn cache_feed_to_disk(cache_path: &Path) -> impl Fn(&str, &RssOrAtomFeed) -> Result<(), Error> {
    use std::fs::OpenOptions;

    let cache_path = cache_path.to_owned();

    move |feed_name: &str, feed: &RssOrAtomFeed| {
        let cache_file_path = cache_path.join(feed_name);
        let cache_file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&cache_file_path)
            .map_err(|err| {
                Error::new(ErrorKind::IoErr(err)).with_data(format!("feed[{}]", feed_name))
            })?;

        log::debug!(
            "writing cache for feed[{}] to {}",
            feed_name,
            cache_file_path.display()
        );

        match feed {
            RssOrAtomFeed::Rss2(channel) => channel
                .write_to(cache_file)
                .map(|_| ())
                .map_err(|err| Error::new(ErrorKind::RssErr(err))),
            RssOrAtomFeed::Atom(feed) => feed
                .write_to(cache_file)
                .map(|_| ())
                .map_err(|err| Error::new(ErrorKind::AtomErr(err.to_string()))),
        }
    }
}

/// Handle the lookup of and caching of an individual feed.
fn get_and_cache_new_items_from_feed<
    R: FeedCacheReadable,
    F: FeedGettable,
    W: FeedCacheWriteable,
>(
    feed_name: &str,
    feed_url: &Url,
    feed_cache_readable: R,
    fetch_feed: F,
    feed_writer: W,
) -> Result<Vec<String>, Error> {
    let maybe_cached_feed = feed_cache_readable.read_cache(feed_name);

    match maybe_cached_feed {
        // if the cache file exists, load it and return new feed urls
        Ok(cached_feed) => {
            log::debug!("cache file found for {}", feed_name);

            let new_feed = fetch_feed.get_feed(feed_name, feed_url)?;

            let cached_item_links: HashSet<_> = cached_feed.get_links().into_iter().collect();
            let new_item_links: HashSet<_> = new_feed.get_links().into_iter().collect();

            let new_links: Vec<_> = new_item_links
                .difference(&cached_item_links)
                .map(|link| link.to_string())
                .collect();

            feed_writer.write_cache(feed_name, &new_feed)?;
            Ok(new_links)
        }

        // if the cache file doesn't exists, save the cache
        Err(Error {
            kind: ErrorKind::IoErr(err),
            ..
        }) if err.kind() == io::ErrorKind::NotFound => {
            log::debug!("cache file not found for {}", feed_name);

            let new_feed = fetch_feed.get_feed(feed_name, feed_url)?;
            feed_writer.write_cache(feed_name, &new_feed)?;

            Ok(vec![])
        }

        // any other Error should be bubbled up
        Err(err) => Err(err),
    }
}

/// A rss feed checker
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// the directory path to source configuration files
    #[arg(long = "conf-path", env = "RSS_CHECKER_CONF_PATH")]
    conf_path: PathBuf,

    /// the directory path to store all cache files
    #[arg(
        long = "cache-path",
        env = "RSS_CHECKER_CACHE_PATH",
        default_value = ".rss_checker/cache"
    )]
    cache_path: PathBuf,

    /// the directory path to store all cache files
    #[arg(long = "log-level", env = "RUST_LOG", default_value = "error")]
    log_level: Option<LogLevelArg>,
}

fn main() -> ExitCode {
    use env_logger::Builder;

    let args = Args::parse();
    let conf_dir_path = args.conf_path;
    let cache_dir_path = args.cache_path;
    let maybe_log_level = args.log_level;

    let mut logger_builder = Builder::from_default_env();
    if let Some(log_level_arg) = maybe_log_level {
        let level = log_level_arg.into();

        logger_builder.filter_level(level);
    };
    logger_builder.init();

    // create the cache directory pathing
    let maybe_cache_dir_metadata = std::fs::metadata(&cache_dir_path);
    match maybe_cache_dir_metadata {
        Ok(meta) if meta.is_dir() => (),
        Ok(_) => {
            log::error!(
                "cache directory path exists and is not a directory: {:?}",
                &cache_dir_path
            );
            return ExitCode::FAILURE;
        }

        // Attempt to create the directory if it doesn't exist.
        Err(_) => {
            log::debug!("creating cache directory at {:?}", &cache_dir_path);
            if let Err(e) = std::fs::create_dir_all(&cache_dir_path) {
                log::error!("{}", e);
                return ExitCode::FAILURE;
            }
        }
    };

    let feed_mappings = match walker::walk_conf_dir(&conf_dir_path) {
        Ok(mappings) => mappings,
        Err(e) => {
            log::error!("{}", e);
            return ExitCode::FAILURE;
        }
    };

    let fetch_feeds: Vec<_> = feed_mappings
        .par_iter()
        .map(|(feed_name, feed_url)| {
            (
                feed_name,
                get_and_cache_new_items_from_feed(
                    feed_name,
                    feed_url,
                    load_cached_feed_from_disk(&cache_dir_path),
                    get_feed_with_blocking_http_request,
                    cache_feed_to_disk(&cache_dir_path),
                ),
            )
        })
        .collect();

    let mut new_unique_links = BTreeSet::new();
    for (feed_name, maybe_feed) in fetch_feeds {
        match maybe_feed {
            Ok(new_links) => new_unique_links.extend(new_links.into_iter()),
            Err(e) => log::error!("[{}]: {}", feed_name, e),
        }
    }

    for new_link in new_unique_links {
        println!("{}", new_link)
    }

    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::*;

    /// Provides a rss 2.0 feed in xml format locally.
    const MOCK_LOCAL_GOOD_FEED: &str = include_str!("../dev/nginx/www/feed.xml");

    #[allow(unused)]
    struct MockFeedGetter<'data> {
        contents: &'data str,
    }

    impl<'data> MockFeedGetter<'data> {
        fn new(contents: &'data str) -> Self {
            Self { contents }
        }
    }

    impl FeedGettable for MockFeedGetter<'_> {
        fn get_feed(&self, _feed_name: &str, _url: &Url) -> Result<RssOrAtomFeed, Error> {
            Channel::read_from(self.contents.as_bytes())
                .map_err(|err| Error::new(ErrorKind::RssErr(err)))
                .map(RssOrAtomFeed::Rss2)
        }
    }

    #[test]
    fn should_parse_valid_feed() {
        let feed_url = Url::parse("http://example.com/feed.xml").unwrap();
        let feed_name = "test";
        let feed_getter = MockFeedGetter::new(MOCK_LOCAL_GOOD_FEED);

        let channel = feed_getter.get_feed(feed_name, &feed_url).unwrap();
        let channel_items = channel.get_links();

        assert_eq!(channel_items.len(), 3);
    }
}
