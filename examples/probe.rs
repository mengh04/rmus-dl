//! 接口探测工具（开发用）：dump 各家原始响应，看下载前能拿到哪些信息。
//!
//! cargo run --release --example probe -- "晴天"

use rmus_dl::sources::{MusicSource, kugou::Kugou, kuwo::Kuwo};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let keyword = std::env::args().nth(1).unwrap_or_else(|| "晴天".into());

    // 极简 percent-encoding（与 sources/mod.rs 内部实现一致，这里是 example）
    fn urlencode(input: &str) -> String {
        let mut out = String::new();
        for byte in input.as_bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(*byte as char)
                }
                _ => out.push_str(&format!("%{byte:02X}")),
            }
        }
        out
    }

    // ---- kuwo：搜索响应字段 + mobi.s 原始文本 ----
    let kuwo = Kuwo::new(false)?;
    let results = kuwo.search(&keyword).await?;
    println!("== kuwo search: {} results ==", results.len());
    if let Some(first) = results.first() {
        println!("first: {:?} rid={}", first.title, first.song_id);
        // 直接拼 DES 查询走一遍 mobi.s，dump 原始响应
        for format in ["flac", "mp3"] {
            let query = format!(
                "user=0&corp=kuwo&source=kwplayer_ar_5.1.0.0_B_jiakong_vh.apk&p2p=1&type=convert_url2&sig=0&format={format}&rid={}",
                first.song_id
            );
            let q = rmus_dl::sources::kuwo::des::encrypt_query(&query);
            let resp = reqwest::get(format!("http://mobi.kuwo.cn/mobi.s?f=kuwo&q={q}"))
                .await?
                .error_for_status()?;
            let text = resp.text().await?;
            println!("---- mobi.s format={format} ----");
            for line in text.lines() {
                if !line.trim().is_empty() {
                    println!("  {line}");
                }
            }
        }
    }

    // ---- kugou：getSongInfo 单曲信息接口 ----
    let kugou = Kugou::new(false)?;
    let results = kugou.search(&keyword).await?;
    println!("== kugou search: {} results ==", results.len());
    if let Some(first) = results.first() {
        println!("first: {:?} hash={}", first.title, first.song_id);
        let url = format!(
            "https://m.kugou.com/app/i/getSongInfo.php?cmd=playInfo&hash={}",
            first.song_id
        );
        let resp = reqwest::get(&url)
            .await?
            .error_for_status()?
            .text()
            .await?;
        println!("---- getSongInfo ----\n{}", &resp[..resp.len().min(1200)]);
    }

    // ---- migu：搜索响应字段（rateFormats）----
    let url = format!(
        "https://c.musicapp.migu.cn/v1.0/content/search_all.do?text={}&pageNo=1&pageSize=3&searchSwitch=%7B%22song%22%3A1%7D",
        urlencode(&keyword)
    );
    let resp = reqwest::Client::new()
        .get(&url)
        .header("Referer", "https://h5.nf.migu.cn/")
        .header("User-Agent", "Mozilla/5.0 (Linux; Android 14) Mobile")
        .send()
        .await?
        .error_for_status()?;
    let text = resp.text().await?;
    println!("---- migu body (first 2000) ----\n{}", &text[..text.len().min(2000)]);

    Ok(())
}
