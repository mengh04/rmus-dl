use regex::Regex;
use reqwest::{
    Client, Url,
    header::{HeaderMap, HeaderValue},
};
use scraper::{Html, Selector};

fn get_csrf_token(html: String) -> String {
    // 模拟获取 CSRF token 的逻辑
    let re = Regex::new(r#"name="csrf_token"\s+value="([^"]+)""#).unwrap();
    if let Some(captures) = re.captures(&html) {
        return captures[1].to_string();
    }
    panic!("CSRF token not found in the HTML content");
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let keyword = "客官不可以";
    let url = Url::parse(format!("https://www.2t58.com/so/{keyword}.html").as_str()).unwrap();
    let client = Client::builder().cookie_store(true).build().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        "User-Agent",
        HeaderValue::from_static(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 ...",
        ),
    );
    let response = client.get(url).headers(headers).send().await.unwrap();
    let verify_url = response.url().clone();

    let content = response.text().await.unwrap();
    if content.contains("安全人机验证") {
        let csrf_token = get_csrf_token(content);
        println!("csrf token: {csrf_token}");
        let mut headers = HeaderMap::new();
        headers.insert(
            "User-Agent",
            HeaderValue::from_static(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 ...",
            ),
        );
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
            let title = a.text().collect::<Vec<_>>().join("");

            let song_id = href
                .rsplit('/')
                .next()
                .unwrap()
                .strip_suffix(".html")
                .unwrap();
        }

        // Download
        let random_ip = format!(
            "{}.{}.{}.{}",
            rand::random::<u8>(),
            rand::random::<u8>(),
            rand::random::<u8>(),
            rand::random::<u8>()
        );

        let song_id = "ZGhzdm5zd3c";

        let resp = client
            .head(format!(
                "https://www.2t58.com/plug/down.php?ac=music&id={song_id}&k=flac"
            ))
            .header("X-Forwarded-For", random_ip)
            .send()
            .await
            .unwrap();

        let download_url = resp.url().clone();
        let bytes = client.get(download_url).send().await?.bytes().await?;
        tokio::fs::write("temp.flac", &bytes).await?;
    }
    Ok(())
}
