use std::collections::{BTreeSet, HashSet};
use std::fs::OpenOptions;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use rayon::prelude::*;
use reqwest::Url;

mod error;
pub(crate) use error::{Error, ErrorKind};
use rss::Channel;

mod walker;

trait FeedCacheReadable {
    fn read_cache(&self, feed_name: &str) -> Result<Channel, Error>;
}

impl<F> FeedCacheReadable for F
where
    F: Fn(&str) -> Result<Channel, Error>,
{
    fn read_cache(&self, feed_name: &str) -> Result<Channel, Error> {
        (self)(feed_name)
    }
}

trait FeedGettable {
    fn get_feed(&self, feed_name: &str, url: &Url) -> Result<Channel, Error>;
}

impl<F> FeedGettable for F
where
    F: Fn(&str, &Url) -> Result<Channel, Error>,
{
    fn get_feed(&self, feed_name: &str, url: &Url) -> Result<Channel, Error> {
        (self)(feed_name, url)
    }
}

trait FeedCacheWriteable {
    fn write_cache(&self, feed_name: &str, feed: &Channel) -> Result<(), Error>;
}

impl<F> FeedCacheWriteable for F
where
    F: Fn(&str, &Channel) -> Result<(), Error>,
{
    fn write_cache(&self, feed_name: &str, feed: &Channel) -> Result<(), Error> {
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

fn get_feed_with_blocking_http_request(feed_name: &str, url: &Url) -> Result<Channel, Error> {
    let resp = reqwest::blocking::get(url.as_str()).map_err(|err| {
        Error::new(ErrorKind::ReqwestErr(err)).with_data(format!("feed[{}]", feed_name))
    })?;

    let contents = resp.text().map_err(|err| {
        Error::new(ErrorKind::ReqwestErr(err)).with_data(format!("feed[{}]", feed_name))
    })?;

    Channel::read_from(contents.as_bytes()).map_err(|err| Error::new(ErrorKind::RssErr(err)))
}

fn load_cached_feed_from_disk(cache_path: &Path) -> impl Fn(&str) -> Result<Channel, Error> {
    let cache_path = cache_path.to_owned();

    move |feed_name: &str| {
        let cache_file_path = cache_path.join(feed_name);
        let cache_file = OpenOptions::new()
            .read(true)
            .open(cache_file_path)
            .map(BufReader::new)
            .map_err(|err| {
                Error::new(ErrorKind::IoErr(err)).with_data(format!("feed[{}]", feed_name))
            })?;

        Channel::read_from(cache_file).map_err(|err| Error::new(ErrorKind::RssErr(err)))
    }
}

fn cache_feed_to_disk(cache_path: &Path) -> impl Fn(&str, &Channel) -> Result<(), Error> {
    use std::fs::OpenOptions;

    let cache_path = cache_path.to_owned();

    move |feed_name: &str, channel: &Channel| {
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

        channel
            .write_to(cache_file)
            .map(|_| ())
            .map_err(|err| Error::new(ErrorKind::RssErr(err)))
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

            let cached_items = cached_feed.items();
            let new_items = new_feed.items();

            let cached_item_links: HashSet<_> =
                cached_items.iter().flat_map(|item| item.link()).collect();
            let new_item_links: HashSet<_> =
                new_items.iter().flat_map(|item| item.link()).collect();

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
            get_and_cache_new_items_from_feed(
                feed_name,
                feed_url,
                load_cached_feed_from_disk(&cache_dir_path),
                get_feed_with_blocking_http_request,
                cache_feed_to_disk(&cache_dir_path),
            )
        })
        .collect();

    let mut new_unique_links = BTreeSet::new();
    for maybe_feed in fetch_feeds {
        match maybe_feed {
            Ok(new_links) => new_unique_links.extend(new_links.into_iter()),
            Err(e) => log::warn!("{}", e),
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

    impl<'data> FeedGettable for MockFeedGetter<'data> {
        fn get_feed(&self, _feed_name: &str, _url: &Url) -> Result<Channel, Error> {
            Channel::read_from(self.contents.as_bytes())
                .map_err(|err| Error::new(ErrorKind::RssErr(err)))
        }
    }

    #[test]
    fn should_parse_valid_feed() {
        let feed_url = Url::parse("http://example.com/feed.xml").unwrap();
        let feed_name = "test";
        let feed_getter = MockFeedGetter::new(MOCK_LOCAL_GOOD_FEED);

        let channel = feed_getter.get_feed(feed_name, &feed_url).unwrap();
        let channel_items = channel.items();

        assert_eq!(channel_items.len(), 3);
    }
}
