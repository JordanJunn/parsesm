use std::env;
use std::fs;
use std::io::Read;

use ansi_term::Colour;
use reqwest::{Client, ClientBuilder};
use sourcemap::{decode, DecodedMap, RewriteOptions, SourceMap};

fn load_from_reader<R: Read>(mut rdr: R) -> Result<SourceMap, sourcemap::Error> {
    let decoded = decode(&mut rdr);
    if decoded.is_ok() {
        match decoded.unwrap() {
            DecodedMap::Regular(sm) => Ok(sm),
            DecodedMap::Index(idx) => idx.flatten_and_rewrite(&RewriteOptions {
                load_local_source_contents: true,
                ..Default::default()
            }),
            e => Err(sourcemap::Error::IncompatibleSourceMap),
        }
    } else {
        Err(sourcemap::Error::IncompatibleSourceMap)
    }
}

fn write_contents(host: &str, path: &str, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::path::Path;

    let mut host = host.trim_start_matches("http://");
    host = host.trim_start_matches("https://");
    let mut trimmed = path.trim_start_matches("webpack:///");
    trimmed = trimmed.trim_start_matches("./");
    let parsed = Path::new(trimmed);

    // if its a module include the module otherwise write it to out dir
    let out_dir = if let Some(module) = parsed.parent() {
        format!("./out/{}/{}", host, module.to_str().unwrap())
    } else {
        format!("./out/{}", host)
    };
    let file_name = parsed.file_name().expect("failed to get file name");
    // creating dir for source if it doesnt exist
    std::fs::create_dir_all(out_dir.clone())?;

    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(true)
        .open(format!(
            "{}/{}", // ./out/module/file || ./out/file
            out_dir,
            file_name.to_str().expect("failed to get str frmo filename")
        ))?;

    file.write_all(contents.as_bytes())?;

    eprintln!(
        "found original source for module {} and file {} of size {}",
        out_dir,
        file_name.to_str().unwrap(),
        contents.len()
    );
    Ok(())
}

struct ParsesmClient {
    inner: reqwest::Client,
}

impl ParsesmClient {
    pub fn new() -> Self {
        use std::time::Duration;

        let client = Client::builder()
            .use_native_tls()
            .danger_accept_invalid_hostnames(true)
            .danger_accept_invalid_certs(true)
            .pool_max_idle_per_host(5)
            .pool_idle_timeout(Duration::from_secs(15))
            .build()
            .expect("failed to build client");

        Self { inner: client }
    }

    pub async fn extract_map(&self, host: &str) -> std::io::Result<()> {
        use bytes::{Buf, Bytes};

        eprintln!(
            "attempting to find sourcemaps for {}",
            Colour::White.bold().paint(host)
        );
        let resp = self.inner.get(host).send().await;
        match resp {
            Ok(r) => {
                if !r.status().is_success() {
                    return Ok(());
                }

                let body = r.text().await.expect("failed to get body");
                let relative_scripts = Self::find_scripts(&host, &body);
                // needs to be string for colour
                let relative_scripts_len = relative_scripts.len().to_string();
                eprintln!(
                    "found {} relative javascript files",
                    Colour::White.bold().paint(&relative_scripts_len)
                );

                let js_maps = self.fetch_map_files(relative_scripts).await?;
                if js_maps.len() == 0 {
                    println!(
                        "no sourcemaps found for {} javascript files. exiting",
                        Colour::White.bold().paint(&relative_scripts_len)
                    );
                    return Ok(());
                }

                eprintln!(
                    "found {}/{} sourcemaps for javascript files",
                    js_maps.len(),
                    Colour::White.bold().paint(&relative_scripts_len)
                );
                js_maps
                    .into_iter()
                    .filter_map(|m| {
                        let buf = Bytes::from(m.1);
                        load_from_reader(buf.reader()).ok()
                    })
                    .for_each(|sm| {
                        sm.sources()
                            .zip(sm.source_contents())
                            .filter_map(|s| if s.1.is_some() { Some(s) } else { None })
                            .for_each(|s| {
                                // unwrap is okay because we verified its Some above
                                if let Err(e) = write_contents(host, s.0, s.1.unwrap()) {
                                    //todo: log error
                                }
                            });
                    })
            }
            _ => {}
        }

        Ok(())
    }

    pub async fn fetch_map_files(
        &self,
        scripts: Vec<String>,
    ) -> std::io::Result<Vec<(String, String)>> {
        let mut bodies = vec![];
        for s in scripts {
            match self.inner.get(s.clone()).send().await {
                Ok(resp) => {
                    if resp.status().is_success() {
                        let body = resp.text().await.expect("failed to get body");
                        bodies.push((s, body));
                    }
                }
                Err(e) => {
                    //todo: log error
                }
            }
        }

        Ok(bodies)
    }

    pub fn find_scripts(host: &str, body: &str) -> Vec<String> {
        use scraper::{Html, Selector};
        let mut res = vec![];
        let doc = Html::parse_document(body);
        let selector = Selector::parse("script").expect("failed to create selector");

        for e in doc.select(&selector) {
            if let Some(src) = e.value().attr("src") {
                // relative url are only considered as part of the apps sourcemap for now
                if src.starts_with("/") {
                    let src = src.replace(".js", ".js.map");
                    res.push(format!("{}{}", host, src));
                } else {
                    //res.push(src.to_owned());
                }
            }
        }

        res
    }
}

#[tokio::main]
async fn main() {
    let client = ParsesmClient::new();
    let args: Vec<_> = env::args().collect();
    client.extract_map(&args[1]).await;
}
