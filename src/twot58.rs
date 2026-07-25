use std::{io::Cursor, path::PathBuf};

use lofty::{
    config::WriteOptions,
    file::{AudioFile, TaggedFileExt},
    picture::{Picture, PictureType},
};
use regex::Regex;
use reqwest::{
    Url,
    header::{HeaderMap, HeaderValue},
};
use scraper::{Html, Selector};

#[derive(Debug)]
pub struct SearchResult {
    pub title: String,
    song_id: String,
    href: String,
}

fn get_csrf_token(html: String) -> String {
    let re = Regex::new(r#"name="csrf_token"\s+value="([^"]+)""#).unwrap();
    if let Some(captures) = re.captures(&html) {
        return captures[1].to_string();
    }
    panic!("CSRF token not found in the HTML content");
}

pub struct TwoT58 {
    client: reqwest::Client,
}

impl TwoT58 {
    pub fn try_new() -> Result<Self, reqwest::Error> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "User-Agent",
            HeaderValue::from_static(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 ...",
            ),
        );
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .default_headers(headers)
            .build()?;
        Ok(Self { client })
    }

    pub async fn search(&self, keyword: &str) -> anyhow::Result<Vec<SearchResult>> {
        let client = &self.client;
        let url = Url::parse(format!("https://www.2t58.com/so/{keyword}.html").as_str()).unwrap();
        let response = client.get(url).send().await.unwrap();
        let verify_url = response.url().clone();

        let content = response.text().await.unwrap();
        let mut results = Vec::new();
        if content.contains("安全人机验证") {
            let csrf_token = get_csrf_token(content);
            let mut headers = HeaderMap::new();
            headers.insert(
                "Content-Type",
                HeaderValue::from_static("application/x-www-form-urlencoded"),
            );
            headers.insert(
                "Referer",
                HeaderValue::from_str(verify_url.as_str()).unwrap(),
            );
            headers.insert(
                "User-Agent",
                HeaderValue::from_static(
                    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 ...",
                ),
            );
            headers.insert(
                "Origin",
                HeaderValue::from_str(&format!(
                    "{}://{}",
                    verify_url.scheme(),
                    verify_url.host_str().unwrap()
                ))
                .unwrap(),
            );

            let response = client
                .post(verify_url)
                .headers(headers)
                .form(&[
                    ("csrf_token", csrf_token),
                    ("human_check", "on".to_string()),
                ])
                .send()
                .await
                .unwrap();

            let html = response.text().await.unwrap();
            let doc = Html::parse_document(&html);
            let row_sel = Selector::parse(".play_list ul li .name a[href]").unwrap();
            for a in doc.select(&row_sel) {
                let href = a.value().attr("href").unwrap();

                let title = a.text().collect::<Vec<_>>();

                let title = title.join("");

                let song_id = href
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .strip_suffix(".html")
                    .unwrap();

                results.push(SearchResult {
                    title: title.to_string(),
                    song_id: song_id.to_string(),
                    href: href.to_string(),
                })
            }
        }
        Ok(results)
    }

    pub async fn get_download_url(&self, selected_song: &SearchResult) -> anyhow::Result<Url> {
        let random_ip = format!(
            "{}.{}.{}.{}",
            rand::random::<u8>(),
            rand::random::<u8>(),
            rand::random::<u8>(),
            rand::random::<u8>()
        );

        let song_id = &selected_song.song_id;

        let resp = self
            .client
            .head(format!(
                "https://www.2t58.com/plug/down.php?ac=music&id={song_id}&k=flac"
            ))
            .header("X-Forwarded-For", random_ip)
            .send()
            .await
            .unwrap();

        let download_url = resp.url().clone();

        Ok(download_url)
    }

    pub async fn write_metadata(
        &self,
        file_path: &PathBuf,
        selected_song: &SearchResult,
    ) -> anyhow::Result<()> {
        // write lyrics
        let lrc = self
            .client
            .get(format!(
                "https://www.2t58.com/plug/down.php?ac=music&id={}&lk=lrc",
                selected_song.song_id
            ))
            .send()
            .await?
            .text()
            .await?;

        let lrc = lrc
            .strip_prefix("[00:00.00]欢迎来访爱听音乐网 www.2t58.com\r\n")
            .unwrap();

        let detail_html = self
            .client
            .get(format!("https://www.2t58.com{}", selected_song.href))
            .send()
            .await?
            .text()
            .await?;

        let detail_doc = Html::parse_document(&detail_html);
        let cover_selector = Selector::parse("img#mcover").unwrap();

        let cover_src = detail_doc
            .select(&cover_selector)
            .next()
            .and_then(|img| img.value().attr("src"))
            .ok_or_else(|| anyhow::anyhow!("Cover image not found"))?;

        let cover_url = Url::parse(cover_src)?;
        let cover_bytes = self.client.get(cover_url).send().await?.bytes().await?;

        let mut cursor = Cursor::new(cover_bytes.to_vec());
        let mut picture = Picture::from_reader(&mut cursor)?;
        picture.set_pic_type(lofty::picture::PictureType::CoverFront);

        let mut tagged_file = lofty::read_from_path(file_path)?;
        if let Some(tag) = tagged_file.primary_tag_mut() {
            tag.insert_text(lofty::tag::ItemKey::Lyrics, lrc.to_owned());
            tag.remove_picture_type(PictureType::CoverFront);
            tag.push_picture(picture);
            tagged_file.save_to_path(file_path, WriteOptions::default())?;
        } else {
            anyhow::bail!("No primary tag found in the FLAC file");
        }

        Ok(())
    }
}
