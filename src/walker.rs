use std::collections::BTreeMap;
use std::fs::DirEntry;
use std::path::Path;

use reqwest::Url;

#[derive(Debug)]
pub struct FeedUrl {
    name: String,
    url: Url,
}

fn walk_files_in_dir<P: AsRef<Path>>(
    conf_dir: P,
) -> std::io::Result<impl Iterator<Item = DirEntry>> {
    use std::fs;

    let dir_contents = fs::read_dir(conf_dir)?;

    let files_in_dir = dir_contents.flatten().filter(|entry| {
        let Ok(metadata) = entry.metadata() else {
            return false;
        };

        metadata.is_file()
    });

    Ok(files_in_dir)
}

pub(crate) fn walk_conf_dir<P>(conf_dir: P) -> Result<BTreeMap<String, Url>, crate::Error>
where
    P: AsRef<Path>,
{
    let files_in_dir = walk_files_in_dir(conf_dir)
        .map_err(|err| crate::Error::new(crate::ErrorKind::IoErr(err)))?;

    let mut feed_urls = BTreeMap::new();
    for entry in files_in_dir {
        let file_name = entry
            .file_name()
            .into_string()
            .map_err(|filename| crate::Error::new(crate::ErrorKind::InvalidFilename(filename)))?;
        let path = entry.path();

        let maybe_path_contents = std::fs::read_to_string(&path)
            .map_err(|err| crate::Error::new(crate::ErrorKind::IoErr(err)));

        let maybe_url = maybe_path_contents.and_then(|contents| {
            let trimmed_contents = contents.as_str().trim();

            Url::parse(trimmed_contents).map_err(|err| {
                crate::Error::new(crate::ErrorKind::InvalidUrl {
                    reason: err,
                    url: trimmed_contents.to_string(),
                })
            })
        });

        let feed_url = maybe_url.map(|url| FeedUrl {
            name: file_name,
            url,
        })?;

        let feed_already_defined = feed_urls
            .insert(feed_url.name.clone(), feed_url.url)
            .is_some();

        if feed_already_defined {
            return Err(crate::Error::new(crate::ErrorKind::DuplicateFeed(
                feed_url.name,
            )));
        }
    }

    Ok(feed_urls)
}
